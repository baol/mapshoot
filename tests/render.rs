use mapshoot::{
    Basemap, Bounds, Cache, ErrorKind, MAX_LATITUDE, Options, Renderer, Style, Viewport, render,
};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tiny_skia::{Color, Pixmap};

fn options() -> Options {
    let mut options = Options::new(
        Bounds::new(-0.01, -0.01, 0.01, 0.01).unwrap(),
        Basemap::None,
    );
    options.width = 256;
    options.height = Some(256);
    options
}

fn rgb(image: &Pixmap, x: u32, y: u32) -> [u8; 3] {
    let p = image.pixel(x, y).unwrap().demultiply();
    [p.red(), p.green(), p.blue()]
}

#[test]
fn bounds_radius_and_projection() {
    let area = Bounds::from_center(0.0, 0.0, 1000.0).unwrap();
    assert!((area.east - 0.0089932).abs() < 0.000001);
    assert!((area.north - 0.0089932).abs() < 0.000001);
    for b in [
        [0.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 1.0, 1.0],
        [0.0, 0.0, 1.0, 91.0],
        [0.0, 0.0, 1.0, 86.0],
        [0.0, 0.0, 181.0, 1.0],
        [180.0, 0.0, -180.0, 1.0],
        [0.0, 0.0, f64::NAN, 1.0],
    ] {
        assert!(Bounds::new(b[0], b[1], b[2], b[3]).is_err(), "{b:?}");
    }
    // Crossing the antimeridian and reaching the Mercator edge are ordinary
    // requests.
    let crossing = Bounds::new(179.0, -1.0, -179.0, 1.0).unwrap();
    assert!(crossing.west > crossing.east);
    let whole = Bounds::new(-180.0, -1.0, 180.0, 1.0).unwrap();
    let mut whole_opts = options();
    whole_opts.bounds = whole;
    whole_opts.width = 512;
    whole_opts.height = Some(512);
    let whole_view = Viewport::new(&whole_opts).unwrap();
    assert_eq!((whole_view.zoom, whole_view.world_size), (1, 512.0));
    assert!(Bounds::new(0.0, 0.0, 1.0, MAX_LATITUDE).is_ok());
    for (lon, lat, radius) in [
        (0.0, 0.0, -1.0),
        (0.0, 0.0, f64::INFINITY),
        (0.0, 0.0, 2e6),
        (0.0, 91.0, 1000.0),
        (181.0, 0.0, 1000.0),
    ] {
        assert!(
            Bounds::from_center(lon, lat, radius).is_err(),
            "{lon},{lat}"
        );
    }
    let wrapped = Bounds::from_center(179.99, 0.0, 10_000.0).unwrap();
    assert!(wrapped.west > wrapped.east, "{wrapped:?}");
    let polar = Bounds::from_center(0.0, 89.0, 10_000.0).unwrap();
    assert!((polar.north - MAX_LATITUDE).abs() < 1e-9, "{polar:?}");
    let cap = Bounds::from_center(0.0, 85.0, 1_000_000.0).unwrap();
    assert_eq!((cap.west, cap.east), (-180.0, 180.0));
    let opts = options();
    let view = Viewport::new(&opts).unwrap();
    let center = view.point(0.0, 0.0, 1);
    assert!((center.0 - 128.0).abs() <= 1.0 && (center.1 - 128.0).abs() <= 1.0);
    let nw = view.point(opts.bounds.west, opts.bounds.north, 1);
    let se = view.point(opts.bounds.east, opts.bounds.south, 1);
    // The box fills the frame exactly, up to pixel rounding.
    assert!(nw.0.abs() <= 1.0 && nw.1.abs() <= 1.0, "{nw:?}");
    assert!(
        (se.0 - 256.0).abs() <= 1.0 && (se.1 - 256.0).abs() <= 1.0,
        "{se:?}"
    );
    assert!(view.point(0.0, 0.005, 1).1 < center.1);
}

#[test]
fn geojson_bounds_use_the_shortest_arc_and_accept_margins() {
    let polygon = json!({
        "type": "Polygon",
        "coordinates": [[[10.0, 40.0], [20.0, 40.0], [20.0, 50.0], [10.0, 50.0], [10.0, 40.0]]]
    });
    let bounds = Bounds::from_geojson(&polygon, 0.0).unwrap();
    assert_eq!(
        (bounds.west, bounds.south, bounds.east, bounds.north),
        (10.0, 40.0, 20.0, 50.0)
    );

    // A crossing line frames the 20-degree arc across the antimeridian.
    let crossing = json!({"type": "LineString", "coordinates": [[170.0, 0.0], [-170.0, 1.0]]});
    let bounds = Bounds::from_geojson(&crossing, 0.0).unwrap();
    assert!(bounds.west > bounds.east, "{bounds:?}");
    assert!((bounds.east - bounds.west + 360.0 - 20.0).abs() < 1e-9);

    // A point has no extent until a margin is added.
    let point = json!({"type": "Point", "coordinates": [13.405, 52.52]});
    assert!(Bounds::from_geojson(&point, 0.0).is_err());
    let padded = Bounds::from_geojson(&point, 1000.0).unwrap();
    assert!(padded.west < 13.405 && padded.east > 13.405, "{padded:?}");
    assert!(padded.south < 52.52 && padded.north > 52.52, "{padded:?}");

    for empty in [
        json!({"type": "FeatureCollection", "features": []}),
        json!({"type": "Feature", "properties": {}, "geometry": null}),
        json!({"type": "GeometryCollection", "geometries": []}),
    ] {
        assert!(Bounds::from_geojson(&empty, 1000.0).is_err(), "{empty}");
    }
    assert!(Bounds::from_geojson(&json!({"type": "Bogus"}), 0.0).is_err());
    assert!(Bounds::from_geojson(&polygon, -1.0).is_err());
    assert!(Bounds::from_geojson(&polygon, 1_000_001.0).is_err());
}

#[test]
fn geojson_framing_includes_marker_graphics() {
    let mut opts = options();
    opts.width = 400;
    opts.height = None;
    opts.geojson = Some(json!({
        "type": "FeatureCollection",
        "features": [
            {
                "type": "Feature",
                "properties": {},
                "geometry": {"type": "Point", "coordinates": [0, 0]}
            },
            {
                "type": "Feature",
                "properties": {"marker-size": "large", "marker-color": "#f00"},
                "geometry": {"type": "Point", "coordinates": [1, 1]}
            }
        ]
    }));
    opts.bounds = Bounds::from_geojson(opts.geojson.as_ref().unwrap(), 0.0).unwrap();
    let coordinates = opts.bounds;
    opts.include_geojson_graphics().unwrap();
    assert!(opts.bounds.west < coordinates.west, "{:?}", opts.bounds);
    assert!(opts.bounds.east > coordinates.east, "{:?}", opts.bounds);
    assert!(opts.bounds.south < coordinates.south, "{:?}", opts.bounds);
    assert!(opts.bounds.north > coordinates.north, "{:?}", opts.bounds);

    let view = Viewport::new(&opts).unwrap();
    let marker = 9.0 + 3.0 / 2.0;
    let (x, y) = view.point(1.0, 1.0, 1);
    assert!((view.width as f32 - x - marker).abs() <= 2.0, "{x}, {y}");
    assert!((y - marker).abs() <= 2.0, "{x}, {y}");
    let (x, y) = view.point(0.0, 0.0, 1);
    assert!((x - marker).abs() <= 2.0, "{x}, {y}");
    assert!((view.height as f32 - y - marker).abs() <= 2.0, "{x}, {y}");
}

#[test]
fn cli_derives_bounds_from_geojson_and_applies_padding() {
    let base = [
        "--geojson",
        "examples/berlin.geojson",
        "--basemap",
        "none",
        "--width",
        "320",
        "-o",
        "-",
    ];
    let result = cli(&base);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let plain = Pixmap::decode_png(&result.stdout).unwrap();
    assert_eq!(plain.width(), 320);

    let result = cli(&[base.as_slice(), &["--padding", "1000"]].concat());
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let padded = Pixmap::decode_png(&result.stdout).unwrap();

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string("examples/berlin.geojson").unwrap()).unwrap();
    let mut expected = options();
    expected.bounds = Bounds::from_geojson(&value, 1000.0).unwrap();
    expected.width = 320;
    expected.height = None;
    expected.geojson = Some(value);
    expected.include_geojson_graphics().unwrap();
    let view = Viewport::new(&expected).unwrap();
    assert_eq!((padded.width(), padded.height()), (view.width, view.height));
    assert_ne!(padded.height(), plain.height());

    // --buffer and --padding are synonyms.
    let by_buffer = cli(&[
        "--geojson",
        "examples/berlin.geojson",
        "--buffer",
        "500",
        "--basemap",
        "none",
        "--width",
        "320",
        "-o",
        "-",
    ]);
    let by_padding = cli(&[
        "--geojson",
        "examples/berlin.geojson",
        "--padding",
        "500",
        "--basemap",
        "none",
        "--width",
        "320",
        "-o",
        "-",
    ]);
    assert!(by_buffer.status.success());
    assert!(by_padding.status.success());
    assert_eq!(by_buffer.stdout, by_padding.stdout);

    let result = cli(&[
        "--geojson",
        "examples/berlin.geojson",
        "--buffer",
        "500",
        "--padding",
        "500",
        "--basemap",
        "none",
        "-o",
        "-",
    ]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(
        message.contains("either --buffer or --padding"),
        "{message}"
    );

    // Explicit bounds apply the margin before the frame is derived.
    let result = cli(&[
        "--bbox",
        "0,0,2,1",
        "--padding",
        "50000",
        "--width",
        "400",
        "--basemap",
        "none",
        "-o",
        "-",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let image = Pixmap::decode_png(&result.stdout).unwrap();
    let mut expected = options();
    expected.bounds = Bounds::new(0.0, 0.0, 2.0, 1.0)
        .unwrap()
        .buffered(50_000.0)
        .unwrap();
    expected.width = 400;
    expected.height = None;
    let view = Viewport::new(&expected).unwrap();
    assert_eq!((image.width(), image.height()), (view.width, view.height));

    // Explicit bounds still require both --center and --radius.
    for (args, needle) in [
        (
            vec![
                "--geojson",
                "examples/berlin.geojson",
                "--center",
                "13.4,52.5",
            ],
            "--center requires --radius",
        ),
        (
            vec!["--geojson", "examples/berlin.geojson", "--radius", "1000"],
            "--radius requires --center",
        ),
        (vec!["--basemap", "none"], "provide --bbox"),
    ] {
        let result = cli(&[args.as_slice(), &["-o", "-"]].concat());
        assert!(!result.status.success(), "{args:?}");
        let message = String::from_utf8_lossy(&result.stderr);
        assert!(message.contains(needle), "{message}");
    }

    for value in ["-1", "1000001", "far"] {
        let result = cli(&[
            "--geojson",
            "examples/berlin.geojson",
            "--padding",
            value,
            "--basemap",
            "none",
            "-o",
            "-",
        ]);
        assert!(!result.status.success(), "{value}");
        let message = String::from_utf8_lossy(&result.stderr);
        assert!(message.contains("--padding"), "{message}");
    }
}

#[test]
fn cli_point_geojson_needs_padding_to_frame() {
    let path = std::env::temp_dir().join(format!("mapshoot-point-{}.geojson", std::process::id()));
    std::fs::write(&path, r#"{"type":"Point","coordinates":[13.405,52.52]}"#).unwrap();
    let path = path.to_str().unwrap();
    let result = cli(&["--geojson", path, "--basemap", "none", "-o", "-"]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains("no extent"), "{message}");
    assert!(message.contains("--padding"), "{message}");

    let result = cli(&[
        "--geojson",
        path,
        "--padding",
        "1000",
        "--basemap",
        "none",
        "-o",
        "-",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    // A buffer alone can also give a degenerate extent a size.
    let result = cli(&[
        "--geojson",
        path,
        "--buffer",
        "1000",
        "--basemap",
        "none",
        "-o",
        "-",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn simplestyle_example_renders() {
    let result = cli(&[
        "--geojson",
        "examples/simplestyle.geojson",
        "--basemap",
        "none",
        "--width",
        "640",
        "-o",
        "-",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let image = Pixmap::decode_png(&result.stdout).unwrap();
    assert_eq!(image.width(), 640);
    let colors: std::collections::HashSet<_> = image
        .pixels()
        .iter()
        .map(|pixel| {
            let pixel = pixel.demultiply();
            (pixel.red(), pixel.green(), pixel.blue())
        })
        .collect();
    assert!(colors.len() > 20, "{} colors", colors.len());
}

#[test]
fn buffer_expands_wraps_and_clamps() {
    let b = Bounds::new(10.0, 40.0, 20.0, 50.0).unwrap();
    let grown = b.buffered(1000.0).unwrap();
    let dy = (1000.0_f64 / 6_371_008.8).to_degrees();
    let dx = dy / 50.0_f64.to_radians().cos();
    assert!((grown.south - (40.0 - dy)).abs() < 1e-9, "{grown:?}");
    assert!((grown.north - (50.0 + dy)).abs() < 1e-9, "{grown:?}");
    // Longitude degrees are shorter near the pole: use the poleward edge.
    assert!((grown.west - (10.0 - dx)).abs() < 1e-9, "{grown:?}");
    assert!((grown.east - (20.0 + dx)).abs() < 1e-9, "{grown:?}");
    assert_eq!(b.buffered(0.0).unwrap().west, b.west);

    let crossing = Bounds::new(179.0, -1.0, -179.0, 1.0)
        .unwrap()
        .buffered(20_000.0)
        .unwrap();
    assert!(crossing.west > crossing.east, "{crossing:?}");

    let polar = Bounds::new(0.0, 80.0, 10.0, 85.0)
        .unwrap()
        .buffered(100_000.0)
        .unwrap();
    assert_eq!(polar.north, MAX_LATITUDE);

    let world = Bounds::new(-180.0, -10.0, 180.0, 10.0)
        .unwrap()
        .buffered(1000.0)
        .unwrap();
    assert_eq!((world.west, world.east), (-180.0, 180.0));

    for bad in [-1.0, f64::NAN, f64::INFINITY, 1_000_001.0] {
        assert!(b.buffered(bad).is_err(), "{bad}");
    }
}

#[test]
fn bounds_aspect_is_projected_not_angular() {
    let square = Bounds::new(-1.0, -1.0, 1.0, 1.0).unwrap();
    assert!((square.aspect() - 1.0).abs() < 1e-4, "{}", square.aspect());
    let wide = Bounds::new(-10.0, 40.0, 10.0, 50.0).unwrap();
    // 20 degrees of longitude are much shorter than 10 degrees of latitude
    // at 45N once projected.
    assert!(wide.aspect() > 1.0, "{}", wide.aspect());
    let crossing = Bounds::new(179.0, -1.0, -179.0, 1.0).unwrap();
    assert!(
        (crossing.aspect() - 1.0).abs() < 1e-3,
        "{}",
        crossing.aspect()
    );
}

#[test]
fn invalid_options_are_errors() {
    let mut cases = Vec::new();
    let mut o = options();
    o.width = 0;
    cases.push(o);
    let mut o = options();
    o.height = Some(8193);
    cases.push(o);
    let mut o = options();
    o.pixel_ratio = 0;
    cases.push(o);
    let mut o = options();
    o.width = 8192;
    o.height = Some(8192);
    o.pixel_ratio = 4;
    cases.push(o);
    let mut o = options();
    o.tile_size = 128;
    cases.push(o);
    let mut o = options();
    o.max_zoom = 23;
    cases.push(o);
    let mut o = options();
    o.concurrency = 0;
    cases.push(o);
    let mut o = options();
    o.timeout = Duration::ZERO;
    cases.push(o);
    let mut o = options();
    o.style.opacity = f32::NAN;
    cases.push(o);
    let mut o = options();
    o.style.color = "red".into();
    cases.push(o);
    let mut o = options();
    o.style.radius = -1.0;
    cases.push(o);
    let mut o = options();
    o.style.weight = f32::INFINITY;
    cases.push(o);
    let mut o = options();
    o.basemap = Basemap::Here {
        api_key: "".into(),
        style: "explore.day".into(),
    };
    cases.push(o);
    let mut o = options();
    o.basemap = Basemap::Here {
        api_key: "secret".into(),
        style: "invalid".into(),
    };
    cases.push(o);
    let mut o = options();
    o.basemap = Basemap::Osm;
    o.max_zoom = 19;
    o.tile_size = 512;
    cases.push(o);
    let mut o = options();
    o.basemap = Basemap::Osm;
    cases.push(o);
    let mut o = options();
    o.basemap = Basemap::Osm;
    o.max_zoom = 19;
    o.concurrency = 8;
    cases.push(o);
    let mut o = options();
    o.basemap = Basemap::Carto {
        api_key: "".into(),
        style: "voyager".into(),
    };
    cases.push(o);
    let mut o = options();
    o.basemap = Basemap::Carto {
        api_key: "secret".into(),
        style: "invalid".into(),
    };
    cases.push(o);
    let mut o = options();
    o.basemap = Basemap::Carto {
        api_key: "secret".into(),
        style: "voyager".into(),
    };
    o.tile_size = 512;
    cases.push(o);
    let mut o = options();
    o.basemap = Basemap::Carto {
        api_key: "secret".into(),
        style: "voyager".into(),
    };
    o.max_zoom = 21;
    cases.push(o);
    let mut o = options();
    o.basemap = Basemap::Xyz {
        url: "file:///{z}/{x}/{y}".into(),
        attribution: "Local".into(),
    };
    cases.push(o);
    let mut o = options();
    o.basemap = Basemap::Xyz {
        url: "http://localhost/{z}/{x}/{y}".into(),
        attribution: "".into(),
    };
    cases.push(o);
    for opts in cases {
        assert!(render(&opts).is_err(), "{opts:?}");
    }
    assert!(serde_json::from_str::<Style>(r##"{"colour":"#ffffff"}"##).is_err());
    // A frame that follows the box aspect can still need an unusable height.
    let mut opts = options();
    opts.bounds = Bounds::new(0.0, 0.0, 0.001, 40.0).unwrap();
    opts.width = 1024;
    opts.height = None;
    assert!(Viewport::new(&opts).is_err());
}

#[test]
fn point_style_output_and_antialiasing() {
    let mut opts = options();
    opts.pixel_ratio = 2;
    opts.geojson = Some(json!({"type":"Point", "coordinates":[0,0]}));
    opts.style = Style {
        color: "#ff0000".into(),
        fill_opacity: 1.0,
        radius: 12.0,
        ..Style::default()
    };
    let image = render(&opts).unwrap();
    assert_eq!((image.width(), image.height()), (512, 512));
    assert_eq!(rgb(&image, 256, 256), [255, 0, 0]);
    assert_eq!(rgb(&image, 10, 10), [221, 221, 221]);
    assert!(
        image
            .pixels()
            .iter()
            .any(|p| p.green() > 0 && p.green() < 221)
    );
    let png = image.encode_png().unwrap();
    let decoded = Pixmap::decode_png(&png).unwrap();
    assert_eq!(decoded.data(), image.data());
}

#[test]
fn simplestyle_feature_marker_color_and_size() {
    let mut opts = options();
    opts.bounds = Bounds::new(-1.0, -1.0, 1.0, 1.0).unwrap();
    opts.geojson = Some(json!({
        "type": "Feature",
        "properties": {
            "marker-color": "#f00",
            "marker-size": "large",
            "stroke": "#00ff00",
            "fill": "#0000ff"
        },
        "geometry": {"type": "Point", "coordinates": [0, 0]}
    }));
    let view = Viewport::new(&opts).unwrap();
    let image = render(&opts).unwrap();
    let (x, y) = view.point(0.0, 0.0, 1);
    let (x, y) = (x as u32, y as u32);
    assert_eq!(rgb(&image, x, y), [255, 0, 0]);
    // Large markers use a 9 px radius; stroke and fill do not affect a marker.
    assert_eq!(rgb(&image, x + 7, y), [255, 0, 0]);
    assert_eq!(rgb(&image, x + 12, y), [221, 221, 221]);
}

#[test]
fn simplestyle_marker_symbols_are_consistently_sized_and_centered() {
    let mut opts = options();
    opts.bounds = Bounds::new(-2.0, -2.0, 2.0, 2.0).unwrap();
    opts.width = 512;
    opts.height = Some(512);
    opts.pixel_ratio = 4;
    let points = [
        (-1.5, 0.5, "small"),
        (-0.5, 0.5, "medium"),
        (0.5, 0.5, "large"),
    ];
    let features: Vec<_> = points
        .iter()
        .map(|(lon, lat, size)| {
            json!({
                "type": "Feature",
                "properties": {
                    "marker-color": "#000000",
                    "marker-size": size,
                    "marker-symbol": "M"
                },
                "geometry": {"type": "Point", "coordinates": [lon, lat]}
            })
        })
        .collect();
    opts.geojson = Some(json!({"type": "FeatureCollection", "features": features}));
    let view = Viewport::new(&opts).unwrap();
    let image = render(&opts).unwrap();
    let mut sizes = Vec::new();
    for (longitude, latitude, _) in points {
        let (x, y) = view.point(longitude, latitude, 4);
        let bounds = white_bounds(&image, x, y).expect("white marker symbol");
        let center_x = (bounds.0 + bounds.2) as f32 / 2.0;
        let center_y = (bounds.1 + bounds.3) as f32 / 2.0;
        assert!(
            (center_x - x).abs() <= 2.0 && (center_y - y).abs() <= 2.0,
            "{bounds:?} centered at {center_x},{center_y}, marker at {x},{y}"
        );
        sizes.push((bounds.2 - bounds.0 + 1, bounds.3 - bounds.1 + 1));
    }
    assert_eq!(sizes[0], sizes[1], "{sizes:?}");
    assert_eq!(sizes[1], sizes[2], "{sizes:?}");
}

fn white_bounds(image: &Pixmap, x: f32, y: f32) -> Option<(u32, u32, u32, u32)> {
    let x = x.round() as i32;
    let y = y.round() as i32;
    let mut bounds = None;
    for py in (y - 60).max(0)..=(y + 60).min(image.height() as i32 - 1) {
        for px in (x - 60).max(0)..=(x + 60).min(image.width() as i32 - 1) {
            let pixel = image.pixel(px as u32, py as u32)?.demultiply();
            if pixel.red() > 245 && pixel.green() > 245 && pixel.blue() > 245 {
                let (left, top, right, bottom) = bounds.unwrap_or((u32::MAX, u32::MAX, 0, 0));
                bounds = Some((
                    left.min(px as u32),
                    top.min(py as u32),
                    right.max(px as u32),
                    bottom.max(py as u32),
                ));
            }
        }
    }
    bounds
}

#[test]
fn simplestyle_applies_to_multi_geometries() {
    let mut opts = options();
    opts.bounds = Bounds::new(-2.0, -2.0, 2.0, 2.0).unwrap();
    opts.geojson = Some(json!({
        "type": "Feature",
        "properties": {"marker-color": "#00f", "marker-size": "small"},
        "geometry": {"type": "MultiPoint", "coordinates": [[-1, -1], [1, 1]]}
    }));
    let view = Viewport::new(&opts).unwrap();
    let image = render(&opts).unwrap();
    for (longitude, latitude) in [(-1.0, -1.0), (1.0, 1.0)] {
        let (x, y) = view.point(longitude, latitude, 1);
        assert_eq!(rgb(&image, x as u32, y as u32), [0, 0, 255]);
    }
}

#[test]
fn simplestyle_feature_styles_lines_and_polygons() {
    let mut opts = options();
    opts.bounds = Bounds::new(-2.0, -2.0, 2.0, 2.0).unwrap();
    opts.width = 256;
    opts.height = Some(256);
    opts.geojson = Some(json!({
        "type": "FeatureCollection",
        "features": [
            {
                "type": "Feature",
                "properties": {
                    "stroke": "0f0",
                    "stroke-width": 5,
                    "stroke-opacity": 0.5
                },
                "geometry": {
                    "type": "LineString",
                    "coordinates": [[-1.5, 1.5], [1.5, 1.5]]
                }
            },
            {
                "type": "Feature",
                "properties": {
                    "fill": "#f00",
                    "fill-opacity": 1,
                    "stroke": "#ff0",
                    "stroke-width": 0
                },
                "geometry": {
                    "type": "Polygon",
                    "coordinates": [[
                        [-1.5, -1.5], [1.5, -1.5], [1.5, -0.5],
                        [-1.5, -0.5], [-1.5, -1.5]
                    ]]
                }
            }
        ]
    }));
    let view = Viewport::new(&opts).unwrap();
    let image = render(&opts).unwrap();
    let (x, y) = view.point(0.0, 1.5, 1);
    let line = rgb(&image, x as u32, y as u32);
    assert!(line[1] > 230 && line[0] < 130 && line[2] < 130, "{line:?}");
    let (x, y) = view.point(0.0, -1.0, 1);
    assert_eq!(rgb(&image, x as u32, y as u32), [255, 0, 0]);
}

#[test]
fn simplestyle_unset_properties_keep_the_global_style() {
    let mut opts = options();
    opts.bounds = Bounds::new(-2.0, -2.0, 2.0, 2.0).unwrap();
    opts.style = Style {
        color: "#ff0000".into(),
        fill_opacity: 1.0,
        ..Style::default()
    };
    opts.geojson = Some(json!({
        "type": "FeatureCollection",
        "features": [
            {
                "type": "Feature",
                "properties": {"fill": "#0f0"},
                "geometry": {
                    "type": "Polygon",
                    "coordinates": [[
                        [-1.5, -1.5], [1.5, -1.5], [1.5, -0.5],
                        [-1.5, -0.5], [-1.5, -1.5]
                    ]]
                }
            },
            {
                "type": "Feature",
                "properties": {"stroke-opacity": 0.5},
                "geometry": {
                    "type": "LineString",
                    "coordinates": [[-1.5, 1.5], [1.5, 1.5]]
                }
            }
        ]
    }));
    let view = Viewport::new(&opts).unwrap();
    let image = render(&opts).unwrap();
    let (x, y) = view.point(0.0, -1.0, 1);
    assert_eq!(rgb(&image, x as u32, y as u32), [0, 255, 0]);
    let (x, y) = view.point(0.0, 1.5, 1);
    let line = rgb(&image, x as u32, y as u32);
    assert!(line[0] > 230 && line[1] < 130 && line[2] < 130, "{line:?}");
}

#[test]
fn simplestyle_marker_and_path_properties_are_independent() {
    let mut opts = options();
    opts.bounds = Bounds::new(-2.0, -2.0, 2.0, 2.0).unwrap();
    opts.style = Style {
        color: "#ff0000".into(),
        fill_opacity: 1.0,
        weight: 1.0,
        ..Style::default()
    };
    opts.geojson = Some(json!({
        "type": "Feature",
        "properties": {
            "marker-color": "#0f0",
            "stroke": "#00f",
            "stroke-width": 9,
            "fill": "#ff0"
        },
        "geometry": {
            "type": "GeometryCollection",
            "geometries": [
                {"type": "Point", "coordinates": [-1, 0]},
                {"type": "LineString", "coordinates": [[0, -1], [0, 1]]}
            ]
        }
    }));
    let view = Viewport::new(&opts).unwrap();
    let image = render(&opts).unwrap();
    let (x, y) = view.point(-1.0, 0.0, 1);
    assert_eq!(rgb(&image, x as u32, y as u32), [0, 255, 0]);
    let (x, y) = view.point(0.0, 0.0, 1);
    assert_eq!(rgb(&image, x as u32, y as u32), [0, 0, 255]);
}

#[test]
fn invalid_simplestyle_properties_are_errors() {
    for (properties, expected) in [
        (json!({"marker-size": "huge"}), "marker-size"),
        (json!({"marker-color": "#12"}), "marker-color"),
        (json!({"stroke": "blue"}), "stroke"),
        (json!({"stroke-opacity": 2}), "stroke-opacity"),
        (json!({"fill-opacity": -0.1}), "fill-opacity"),
        (json!({"stroke-width": -1}), "stroke-width"),
        (json!({"stroke-width": 2000}), "stroke-width"),
        (json!({"marker-symbol": 5}), "marker-symbol"),
    ] {
        let mut opts = options();
        opts.geojson = Some(json!({
            "type": "Feature",
            "properties": properties,
            "geometry": {"type": "Point", "coordinates": [0, 0]}
        }));
        let message = render(&opts).unwrap_err().to_string();
        assert!(message.contains(expected), "{message}");
        assert!(message.contains("properties."), "{message}");
    }
    let mut opts = options();
    opts.geojson = Some(json!({
        "type": "Feature",
        "properties": [],
        "geometry": {"type": "Point", "coordinates": [0, 0]}
    }));
    assert!(render(&opts).is_err());
    // Null properties and unsupported icon IDs are ignored.
    for properties in [
        json!(null),
        json!({"marker-color": null, "stroke": null}),
        json!({"marker-symbol": "bus"}),
    ] {
        let mut opts = options();
        opts.geojson = Some(json!({
            "type": "Feature",
            "properties": properties,
            "geometry": {"type": "Point", "coordinates": [0, 0]}
        }));
        assert!(render(&opts).is_ok(), "{properties}");
    }
}

#[test]
fn polygon_hole_is_independent_of_ring_winding() {
    let mut opts = options();
    opts.geojson = Some(json!({"type":"Polygon", "coordinates":[
        [[-0.009,-0.009],[0.009,-0.009],[0.009,0.009],[-0.009,0.009],[-0.009,-0.009]],
        [[-0.002,-0.002],[0.002,-0.002],[0.002,0.002],[-0.002,0.002],[-0.002,-0.002]]
    ]}));
    opts.style = Style {
        color: "#0000ff".into(),
        fill_opacity: 1.0,
        weight: 0.0,
        ..Style::default()
    };
    let image = render(&opts).unwrap();
    assert_eq!(rgb(&image, 128, 128), [221, 221, 221]);
    assert_eq!(rgb(&image, 160, 128), [0, 0, 255]);
}

#[test]
fn polygon_ring_closure_compares_numeric_coordinates() {
    let mut opts = options();
    opts.geojson = Some(
        serde_json::from_str(
            r#"{"type":"Polygon","coordinates":[[[0,0],[0.005,0],[0.005,0.005],[0.0,0.0]]]}"#,
        )
        .unwrap(),
    );
    assert!(render(&opts).is_ok());
}

#[test]
fn line_clipping_and_alpha() {
    let mut opts = options();
    opts.geojson = Some(json!({"type":"LineString", "coordinates":[[-1,0],[1,0]]}));
    opts.style = Style {
        color: "#ff0000".into(),
        weight: 10.0,
        opacity: 0.5,
        ..Style::default()
    };
    let image = render(&opts).unwrap();
    for x in [0, 128, 255] {
        let p = rgb(&image, x, 128);
        assert!(
            (237..=239).contains(&p[0]) && (109..=111).contains(&p[1]),
            "{p:?}"
        );
    }
}

#[test]
fn all_geojson_containers_render() {
    let mut opts = options();
    opts.geojson = Some(json!({"type":"FeatureCollection", "features":[
        {"type":"Feature","properties":{},"geometry":null},
        {"type":"Feature","properties":{},"geometry":{"type":"GeometryCollection","geometries":[
            {"type":"MultiPoint","coordinates":[[0,0],[0.004,0]]},
            {"type":"MultiLineString","coordinates":[[[-0.005,-0.005],[0.005,-0.005]]]},
            {"type":"MultiPolygon","coordinates":[[[[-0.005,0.005],[0.005,0.005],[0.005,0.009],[-0.005,0.005]]]]}
        ]}}
    ]}));
    opts.style.fill_opacity = 1.0;
    let image = render(&opts).unwrap();
    assert_eq!(rgb(&image, 128, 128), [51, 136, 255]);
}

#[test]
fn malformed_geojson_is_not_silently_ignored() {
    for value in [
        json!(null),
        json!({}),
        json!({"type":"Bogus"}),
        json!({"type":"Feature"}),
        json!({"type":"FeatureCollection","features":[null]}),
        json!({"type":"Point","coordinates":[0]}),
        json!({"type":"Point","coordinates":[0,91]}),
        json!({"type":"Point","coordinates":[0,0,"bad"]}),
        json!({"type":"LineString","coordinates":[[0,0]]}),
        json!({"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,1]]]}),
        json!({"type":"GeometryCollection","geometries":[{"type":"Feature","geometry":null}]}),
    ] {
        let mut opts = options();
        opts.geojson = Some(value);
        assert!(render(&opts).is_err(), "{opts:?}");
    }
    let mut opts = options();
    opts.geojson = Some(json!({"type":"FeatureCollection", "features":[]}));
    assert!(render(&opts).is_ok());
}

#[derive(Default)]
struct Log {
    paths: Mutex<Vec<String>>,
    agents: Mutex<Vec<String>>,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

#[derive(Clone, Copy)]
enum Body {
    /// A solid PNG tile of the given pixel size.
    Png(u32),
    /// Bytes that are not a PNG at all.
    Corrupt,
    /// One byte more than the accepted tile size cap.
    Oversize,
}

struct Fixture {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    log: Arc<Log>,
}

impl Fixture {
    fn new(status: u16, delay: Duration, corrupt: bool) -> Self {
        Self::with_body(
            status,
            delay,
            if corrupt {
                Body::Corrupt
            } else {
                Body::Png(256)
            },
        )
    }

    fn with_body(status: u16, delay: Duration, body: Body) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Log::default());
        let thread = {
            let stop = stop.clone();
            let log = log.clone();
            std::thread::spawn(move || {
                let mut workers = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let log = log.clone();
                            workers.push(std::thread::spawn(move || {
                                serve(stream, status, delay, body, &log);
                            }));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1))
                        }
                        Err(e) => panic!("test server failed: {e}"),
                    }
                }
                for worker in workers {
                    worker.join().unwrap();
                }
            })
        };
        Self {
            url: format!("http://{addr}/{{z}}/{{x}}/{{y}}.png"),
            stop,
            thread: Some(thread),
            log,
        }
    }

    fn options(&self) -> Options {
        let mut opts = options();
        opts.bounds = Bounds::new(-180.0, -MAX_LATITUDE, 180.0, MAX_LATITUDE).unwrap();
        opts.width = 512;
        opts.height = Some(512);
        opts.basemap = Basemap::Xyz {
            url: self.url.clone(),
            attribution: "Test".into(),
        };
        opts
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn serve(mut stream: TcpStream, status: u16, delay: Duration, body: Body, log: &Log) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut request = Vec::new();
    let mut byte = [0];
    while !request.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).expect("read fixture request") == 0 {
            return;
        }
        request.push(byte[0]);
        assert!(request.len() < 16384);
    }
    let text = String::from_utf8(request).unwrap();
    let path = text.split_whitespace().nth(1).unwrap().to_string();
    log.paths.lock().unwrap().push(path.clone());
    let agent = text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("user-agent")
                .then(|| value.trim().to_string())
        })
        .unwrap_or_default();
    log.agents.lock().unwrap().push(agent);
    let current = log.active.fetch_add(1, Ordering::SeqCst) + 1;
    log.max_active.fetch_max(current, Ordering::SeqCst);
    std::thread::sleep(delay);
    let parts: Vec<_> = path.split('/').collect();
    let x: u8 = parts[2].parse().unwrap_or(0);
    let y: u8 = parts[3].split('.').next().unwrap().parse().unwrap_or(0);
    let body = match body {
        Body::Png(size) => {
            let mut tile = Pixmap::new(size, size).unwrap();
            tile.fill(Color::from_rgba8(
                x.saturating_mul(100),
                y.saturating_mul(100),
                50,
                255,
            ));
            tile.encode_png().unwrap()
        }
        Body::Corrupt => b"not a PNG".to_vec(),
        Body::Oversize => vec![0; 8 * 1024 * 1024 + 1],
    };
    let header = format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream
        .write_all(header.as_bytes())
        .and_then(|_| stream.write_all(&body));
    log.active.fetch_sub(1, Ordering::SeqCst);
}

#[test]
fn tiles_are_stitched_at_correct_positions_with_bounded_concurrency() {
    let server = Fixture::new(200, Duration::from_millis(40), false);
    let mut opts = server.options();
    opts.concurrency = 2;
    let image = render(&opts).unwrap();
    assert_eq!(rgb(&image, 10, 10), [0, 0, 50]);
    assert_eq!(rgb(&image, 300, 10), [100, 0, 50]);
    assert_eq!(rgb(&image, 10, 300), [0, 100, 50]);
    assert_eq!(rgb(&image, 300, 300), [100, 100, 50]);
    let mut paths = server.log.paths.lock().unwrap().clone();
    paths.sort();
    assert_eq!(
        paths,
        ["/1/0/0.png", "/1/0/1.png", "/1/1/0.png", "/1/1/1.png"]
    );
    let max_active = server.log.max_active.load(Ordering::SeqCst);
    assert!((1..=2).contains(&max_active), "{max_active}");
    let agents = server.log.agents.lock().unwrap().clone();
    assert!(!agents.is_empty());
    assert!(
        agents
            .iter()
            .all(|ua| ua.starts_with(&format!("mapshoot/{}", env!("CARGO_PKG_VERSION")))),
        "{agents:?}"
    );
    // Attribution is rendered above overlays, within the final image.
    assert_ne!(rgb(&image, 500, 500), [100, 100, 50]);
}

#[test]
fn tile_crop_pixel_ratio_and_projection_align() {
    let server = Fixture::new(200, Duration::ZERO, false);
    let mut opts = server.options();
    opts.width = 300;
    opts.height = Some(300);
    opts.pixel_ratio = 2;
    opts.bounds = Bounds::new(-20.0, -20.0, 20.0, 20.0).unwrap();
    opts.geojson = Some(json!({"type":"Point","coordinates":[0,0]}));
    opts.style = Style {
        color: "#ff0000".into(),
        fill_opacity: 1.0,
        ..Style::default()
    };
    let view = Viewport::new(&opts).unwrap();
    let image = render(&opts).unwrap();
    assert_eq!((image.width(), image.height()), (600, 600));
    let center = view.point(0.0, 0.0, 2);
    assert_eq!(rgb(&image, center.0 as u32, center.1 as u32), [255, 0, 0]);
    // Equator and prime meridian are tile boundaries at every zoom >= 1.
    let half = (1u32 << view.zoom) / 2;
    assert_eq!(
        rgb(&image, 10, 10),
        [
            ((half - 1) * 100).min(255) as u8,
            ((half - 1) * 100).min(255) as u8,
            50
        ]
    );
}

#[test]
fn bbox_frames_exactly_and_fits_when_the_frame_differs() {
    let mut opts = options();
    opts.bounds = Bounds::new(-90.0, -45.0, 90.0, 45.0).unwrap();
    opts.width = 400;
    opts.height = None;
    opts.geojson = Some(json!({"type": "Polygon", "coordinates": [
        [[-90, -45], [90, -45], [90, 45], [-90, 45], [-90, -45]]
    ]}));
    opts.style = Style {
        color: "#00ff00".into(),
        fill_opacity: 1.0,
        weight: 0.0,
        ..Style::default()
    };
    let view = Viewport::new(&opts).unwrap();
    // The height follows the projected box aspect.
    assert_eq!((view.width, view.height), (400, 224));
    let nw = view.point(-90.0, 45.0, 1);
    let se = view.point(90.0, -45.0, 1);
    assert!(nw.0.abs() < 1.0 && nw.1.abs() < 1.0, "{nw:?}");
    assert!(
        (se.0 - 400.0).abs() < 1.0 && (se.1 - 224.0).abs() < 1.0,
        "{se:?}"
    );
    let image = render(&opts).unwrap();
    assert_eq!((image.width(), image.height()), (400, 224));
    // The frame is the box: no margin anywhere.
    for y in 1..image.height() - 1 {
        for x in 1..image.width() - 1 {
            assert_eq!(rgb(&image, x, y), [0, 255, 0], "at {x},{y}");
        }
    }

    // A frame with another aspect ratio fits the box inside and shows the
    // surrounding map instead of cropping the box.
    opts.height = Some(400);
    let view = Viewport::new(&opts).unwrap();
    let nw = view.point(-90.0, 45.0, 1);
    let se = view.point(90.0, -45.0, 1);
    assert!(nw.1 > 80.0 && se.1 < 320.0, "{nw:?} {se:?}");
    let image = render(&opts).unwrap();
    assert_eq!(rgb(&image, 200, 10), [221, 221, 221]);
    assert_eq!(rgb(&image, 200, 200), [0, 255, 0]);
}

#[test]
fn fractional_zoom_scales_tiles_without_background_gaps() {
    let server = Fixture::new(200, Duration::ZERO, false);
    let mut opts = server.options();
    opts.width = 300;
    opts.height = None;
    opts.bounds = Bounds::new(-90.0, -45.0, 90.0, 45.0).unwrap();
    let view = Viewport::new(&opts).unwrap();
    assert!(view.scale > 0.5 && view.scale < 1.0, "{}", view.scale);
    let image = render(&opts).unwrap();
    assert_eq!((image.width(), image.height()), (300, 168));
    // Resampling must not leave unpainted rows between tile rows.
    for y in 0..image.height() {
        assert_ne!(rgb(&image, 200, y), [221, 221, 221], "gap at y={y}");
    }
    // Tiles north and south of the equator keep their own color.
    assert_eq!(rgb(&image, 200, 78), [200, 100, 50]);
    assert_eq!(rgb(&image, 200, 90), [200, 200, 50]);
}

#[test]
fn world_wrap_and_polar_background_do_not_request_invalid_tiles() {
    let server = Fixture::new(200, Duration::ZERO, false);
    let mut opts = server.options();
    opts.width = 512;
    opts.height = Some(600);
    opts.max_zoom = 0;
    let image = render(&opts).unwrap();
    assert_eq!((image.width(), image.height()), (512, 600));
    assert_eq!(rgb(&image, 10, 256), [0, 0, 50]);
    assert_eq!(rgb(&image, 500, 256), [0, 0, 50]);
    assert_eq!(rgb(&image, 256, 10), [221, 221, 221]);
    assert_eq!(rgb(&image, 256, 590), [221, 221, 221]);
    // The one tile needed for the whole row is requested exactly once, then
    // reused at both world offsets.
    assert_eq!(*server.log.paths.lock().unwrap(), ["/0/0/0.png"]);
}

#[test]
fn request_errors_invalid_tiles_and_timeouts_are_explicit_and_redacted() {
    for (status, corrupt, expected) in [(503, false, "HTTP 503"), (200, true, "PNG tile")] {
        let server = Fixture::new(status, Duration::ZERO, corrupt);
        let mut opts = server.options();
        if let Basemap::Xyz { url, .. } = &mut opts.basemap {
            url.push_str("?apiKey=TOPSECRET");
        }
        let error = render(&opts).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("TOPSECRET") && !error.contains("http://"));
    }
    let server = Fixture::new(200, Duration::from_millis(200), false);
    let mut opts = server.options();
    opts.timeout = Duration::from_millis(50);
    let start = Instant::now();
    let error = render(&opts).unwrap_err().to_string();
    assert!(
        error.contains("timeout") || error.contains("deadline"),
        "{error}"
    );
    assert!(start.elapsed() < Duration::from_millis(190));
}

#[test]
fn deadline_covers_all_tile_requests_not_just_individual_ones() {
    let server = Fixture::new(200, Duration::from_millis(70), false);
    let mut opts = server.options();
    opts.concurrency = 1;
    opts.timeout = Duration::from_millis(100);
    assert!(render(&opts).is_err());
    assert!(server.log.paths.lock().unwrap().len() < 4);
}

#[test]
fn invalid_attribution_is_reported_before_network_access() {
    let server = Fixture::new(200, Duration::ZERO, false);
    for text in ["", "Copyright ©", &"a".repeat(100)] {
        let mut opts = server.options();
        opts.basemap = Basemap::Xyz {
            url: server.url.clone(),
            attribution: text.into(),
        };
        assert!(render(&opts).is_err());
    }
    assert!(server.log.paths.lock().unwrap().is_empty());
}

fn test_cache_dir() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mapshoot-test-cache-{}-{nanos}",
            std::process::id()
        ))
    })
}

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mapshoot"));
    command.env("MAPSHOOT_CACHE_DIR", test_cache_dir());
    command
}

fn cli(args: &[&str]) -> Output {
    command().args(args).output().unwrap()
}

#[test]
fn cli_help_validation_and_png_stdout() {
    let help = cli(&["--help"]);
    assert!(help.status.success());
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(help_text.contains("longitude,latitude"));
    assert!(help_text.contains("here|osm|carto|xyz|none"), "{help_text}");
    assert!(help_text.contains("Default: osm (no key)"), "{help_text}");
    assert!(help_text.contains("--carto-style"), "{help_text}");
    assert!(help_text.contains("MAPSHOOT_CARTO_API_KEY"), "{help_text}");
    assert!(help_text.contains("MAPSHOOT_HERE_API_KEY"), "{help_text}");
    assert!(help_text.contains("voyager_labels_under"), "{help_text}");
    assert!(help_text.contains("logistics.satellite.day"), "{help_text}");
    assert!(help_text.contains("default: 19 for osm"), "{help_text}");
    assert!(help_text.contains("default: 2 for osm"), "{help_text}");
    for args in [
        vec!["--bogus"],
        vec!["--bbox", "0,,1,1", "-o", "-", "--basemap", "none"],
        vec!["--bbox", "0,0,1,1", "--center", "0,0", "-o", "-"],
        vec!["--bbox", "0,0,1,1", "-o", "file.jpg"],
        vec![
            "--bbox",
            "0,0,1,1",
            "-o",
            "-",
            "--basemap",
            "none",
            "--width",
            "0",
        ],
        vec![
            "--bbox",
            "0,0,1,1",
            "-o",
            "-",
            "--basemap",
            "none",
            "--width",
            "--height",
        ],
        vec![
            "--bbox",
            "0,0,1,1",
            "-o",
            "-",
            "--basemap",
            "none",
            "--width",
            "256",
            "--width",
            "512",
        ],
    ] {
        let result = cli(&args);
        assert!(!result.status.success(), "{args:?}");
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8_lossy(&result.stderr).starts_with("mapshoot:"));
    }
    let result = cli(&[
        "--center",
        "13.405,52.52",
        "--radius",
        "1500",
        "--basemap",
        "none",
        "--geojson",
        "examples/berlin.geojson",
        "--style",
        "examples/berlin-style.json",
        "--width",
        "320",
        "--height",
        "240",
        "-o",
        "-",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let png = Pixmap::decode_png(&result.stdout).unwrap();
    assert_eq!((png.width(), png.height()), (320, 240));
    assert!(String::from_utf8_lossy(&result.stderr).contains("--basemap none disables map tiles"));
}

#[test]
fn cli_caches_tiles_by_default_and_accepts_cache_flags() {
    let server = Fixture::new(200, Duration::ZERO, false);
    let output =
        std::env::temp_dir().join(format!("mapshoot-cli-cache-{}.png", std::process::id()));
    let output = output.to_str().unwrap();
    let args = [
        "--bbox",
        "-0.01,-0.01,0.01,0.01",
        "--basemap",
        "xyz",
        "--tile-url",
        &server.url,
        "--attribution",
        "Test",
        "--width",
        "256",
        "--height",
        "256",
        "--max-zoom",
        "10",
        "--output",
        output,
    ];
    let first = cli(&args);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let requests = server.log.paths.lock().unwrap().len();
    assert!(requests > 0);
    assert!(Pixmap::decode_png(&std::fs::read(output).unwrap()).is_ok());

    // The shared MAPSHOOT_CACHE_DIR from cli() makes the second render local.
    let second = cli(&args);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(server.log.paths.lock().unwrap().len(), requests);

    let no_cache = cli(&[args.as_slice(), &["--no-cache"]].concat());
    assert!(
        no_cache.status.success(),
        "{}",
        String::from_utf8_lossy(&no_cache.stderr)
    );
    assert!(server.log.paths.lock().unwrap().len() > requests);

    let dir = cache_test_dir("cli-flags");
    let explicit = [
        args.as_slice(),
        &[
            "--cache-dir",
            dir.to_str().unwrap(),
            "--cache-ttl",
            "1",
            "--cache-max-mb",
            "1",
        ],
    ]
    .concat();
    let before = server.log.paths.lock().unwrap().len();
    assert!(cli(&explicit).status.success());
    assert!(server.log.paths.lock().unwrap().len() > before);
    let after = server.log.paths.lock().unwrap().len();
    assert!(cli(&explicit).status.success());
    assert_eq!(server.log.paths.lock().unwrap().len(), after);
    assert!(!cache_files(&dir).is_empty());

    std::fs::remove_file(output).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cli_cache_flags_are_validated() {
    let dir = cache_test_dir("cli-validation");
    let dir_text = dir.to_str().unwrap();
    let common = ["--bbox", "0,0,1,1", "--basemap", "none", "-o", "-"];
    for (extra, message) in [
        (
            vec!["--no-cache", "--cache-dir", dir_text],
            "use either --no-cache or --cache-dir",
        ),
        (
            vec!["--no-cache", "--cache-ttl", "7"],
            "use either --no-cache or --cache-ttl",
        ),
        (
            vec!["--no-cache", "--cache-max-mb", "512"],
            "use either --no-cache or --cache-max-mb",
        ),
        (vec!["--cache-ttl", "0"], "--cache-ttl must be 1..3650"),
        (vec!["--cache-ttl", "far"], "--cache-ttl must be whole days"),
        (
            vec!["--cache-max-mb", "0"],
            "--cache-max-mb must be 1..102400",
        ),
        (
            vec!["--cache-max-mb", "far"],
            "--cache-max-mb must be whole megabytes",
        ),
        (vec!["--no-cache=yes"], "--no-cache does not take a value"),
        (vec!["--cache-dir="], "cache directory must not be empty"),
    ] {
        let args = [common.as_slice(), extra.as_slice()].concat();
        let result = cli(&args);
        assert!(!result.status.success(), "{extra:?}");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains(message), "{extra:?}: {stderr}");
    }
    let help = cli(&["--help"]);
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("--no-cache"), "{help}");
    assert!(help.contains("--cache-ttl"), "{help}");
    assert!(help.contains("--cache-max-mb"), "{help}");
    assert!(help.contains("MAPSHOOT_CACHE_DIR"), "{help}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_missing_here_key_fails_instead_of_producing_a_blank_map() {
    let result = command()
        .env_remove("HERE_API_KEY")
        .env_remove("MAPSHOOT_HERE_API_KEY")
        .args([
            "--bbox",
            "13.37,52.50,13.44,52.54",
            "--basemap",
            "here",
            "-o",
            "-",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains("MAPSHOOT_HERE_API_KEY"), "{message}");
    assert!(message.contains("HERE_API_KEY"), "{message}");
    assert!(message.contains("--basemap osm"), "{message}");
}

#[test]
fn cli_defaults_to_osm_without_a_token() {
    for (extra, expected) in [
        (
            vec!["--max-zoom", "20"],
            "max-zoom must be 0..19 for the OSM basemap",
        ),
        (
            vec!["--concurrency", "8"],
            "concurrency must be 1..2 for the OSM basemap",
        ),
        (vec!["--tile-size", "512"], "OSM preset serves 256 px tiles"),
    ] {
        let result = command()
            .env_remove("HERE_API_KEY")
            .env_remove("MAPSHOOT_HERE_API_KEY")
            .env_remove("CARTO_API_KEY")
            .env_remove("MAPSHOOT_CARTO_API_KEY")
            .args(["--bbox", "13.37,52.50,13.44,52.54", "-o", "-"])
            .args(&extra)
            .output()
            .unwrap();
        assert!(!result.status.success(), "{extra:?}");
        assert!(result.stdout.is_empty(), "{extra:?}");
        let message = String::from_utf8_lossy(&result.stderr);
        assert!(message.contains(expected), "{extra:?}: {message}");
        assert!(!message.contains("API_KEY"), "{extra:?}: {message}");
    }
}

#[test]
fn cli_carto_requires_a_key_and_validates_styles_and_flags() {
    let bbox = ["--bbox", "13.37,52.50,13.44,52.54", "-o", "-"];

    let result = command()
        .env_remove("CARTO_API_KEY")
        .env_remove("MAPSHOOT_CARTO_API_KEY")
        .args([bbox.as_slice(), &["--basemap", "carto"]].concat())
        .output()
        .unwrap();
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains("MAPSHOOT_CARTO_API_KEY"), "{message}");
    assert!(message.contains("CARTO_API_KEY"), "{message}");
    assert!(
        message.contains("https://carto.com/basemaps/apikey"),
        "{message}"
    );

    // The prefixed variable is accepted; the invalid style fails before any
    // network request is attempted.
    for (variable, other) in [
        ("MAPSHOOT_CARTO_API_KEY", "CARTO_API_KEY"),
        ("CARTO_API_KEY", "MAPSHOOT_CARTO_API_KEY"),
    ] {
        let result = command()
            .env_remove("CARTO_API_KEY")
            .env_remove("MAPSHOOT_CARTO_API_KEY")
            .env(variable, "test-key")
            .env_remove(other)
            .args(
                [
                    bbox.as_slice(),
                    &["--basemap", "carto", "--carto-style", "bogus"],
                ]
                .concat(),
            )
            .output()
            .unwrap();
        assert!(!result.status.success());
        let message = String::from_utf8_lossy(&result.stderr);
        assert!(message.contains("unknown CARTO style: bogus"), "{message}");
        assert!(message.contains("voyager"), "{message}");
    }

    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "none",
        "--carto-style",
        "voyager",
        "-o",
        "-",
    ]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(
        message.contains("--carto-style requires --basemap carto"),
        "{message}"
    );

    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "osm",
        "--here-style",
        "explore.day",
        "-o",
        "-",
    ]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(
        message.contains("--here-style requires --basemap here"),
        "{message}"
    );

    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "osm",
        "--tile-url",
        "https://example.com/{z}/{x}/{y}.png",
        "--attribution",
        "Test",
        "-o",
        "-",
    ]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains("require --basemap xyz"), "{message}");

    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "osm",
        "--tile-size",
        "512",
        "-o",
        "-",
    ]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(
        message.contains("OSM preset serves 256 px tiles"),
        "{message}"
    );

    // Flag compatibility is checked before provider keys, so a missing CARTO
    // key must not mask the invalid --here-style.
    let result = command()
        .env_remove("CARTO_API_KEY")
        .env_remove("MAPSHOOT_CARTO_API_KEY")
        .args([
            "--bbox",
            "13.37,52.50,13.44,52.54",
            "--basemap",
            "carto",
            "--here-style",
            "explore.day",
            "-o",
            "-",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(
        message.contains("--here-style requires --basemap here"),
        "{message}"
    );
    assert!(!message.contains("MAPSHOOT_CARTO_API_KEY"), "{message}");

    let result = command()
        .env_remove("CARTO_API_KEY")
        .env_remove("MAPSHOOT_CARTO_API_KEY")
        .args([
            "--bbox",
            "13.37,52.50,13.44,52.54",
            "--basemap",
            "carto",
            "--tile-url",
            "https://example.com/{z}/{x}/{y}.png",
            "-o",
            "-",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains("require --basemap xyz"), "{message}");
    assert!(!message.contains("MAPSHOOT_CARTO_API_KEY"), "{message}");
}

#[test]
fn cli_osm_rejects_zoom_and_concurrency_above_provider_limits() {
    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "osm",
        "--max-zoom",
        "20",
        "-o",
        "-",
    ]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(
        message.contains("max-zoom must be 0..19 for the OSM basemap"),
        "{message}"
    );

    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "osm",
        "--concurrency",
        "8",
        "-o",
        "-",
    ]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(
        message.contains("concurrency must be 1..2 for the OSM basemap"),
        "{message}"
    );
}

#[test]
fn cli_basemap_errors_do_not_create_or_overwrite_images() {
    let path =
        std::env::temp_dir().join(format!("mapshoot-basemap-error-{}.png", std::process::id()));
    for (status, hint) in [
        (401, "API key"),
        (403, "permissions"),
        (404, "tile URL"),
        (429, "rate limit"),
        (503, "provider unavailable"),
    ] {
        let server = Fixture::new(status, Duration::ZERO, false);
        let url = format!("{}?apiKey=TOPSECRET", server.url);
        let args = [
            "--bbox",
            "-0.01,-0.01,0.01,0.01",
            "--basemap",
            "xyz",
            "--tile-url",
            &url,
            "--attribution",
            "Test",
            "-o",
            path.to_str().unwrap(),
        ];
        let result = cli(&args);
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(!path.exists());
        let message = String::from_utf8_lossy(&result.stderr);
        assert!(message.contains("XYZ basemap failed"), "{message}");
        assert!(message.contains(&format!("HTTP {status}")), "{message}");
        assert!(message.contains(hint), "{message}");
        assert!(!message.contains("TOPSECRET"));
        assert!(!message.contains("Saved"));

        std::fs::write(&path, b"existing image").unwrap();
        assert!(!cli(&args).status.success());
        assert_eq!(std::fs::read(&path).unwrap(), b"existing image");
        std::fs::remove_file(&path).unwrap();
    }
}

#[test]
fn cli_errors_name_the_offending_option_or_file() {
    let missing =
        std::env::temp_dir().join(format!("mapshoot-missing-{}.geojson", std::process::id()));
    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "none",
        "--geojson",
        missing.to_str().unwrap(),
        "-o",
        "-",
    ]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains("--geojson"), "{message}");
    assert!(message.contains(missing.to_str().unwrap()), "{message}");

    let manifest = std::env::temp_dir().join(format!("mapshoot-bad-{}.json", std::process::id()));
    std::fs::write(&manifest, r##"{"fillColorr":"#fff"}"##).unwrap();
    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "none",
        "--style",
        manifest.to_str().unwrap(),
        "-o",
        "-",
    ]);
    std::fs::remove_file(&manifest).unwrap();
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains("--style"), "{message}");
    assert!(message.contains(manifest.to_str().unwrap()), "{message}");
    assert!(message.contains("unknown field"), "{message}");

    let result = cli(&["--bbox", "abc", "--basemap", "none", "-o", "-"]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains("--bbox"), "{message}");
    assert!(message.contains("4 comma-separated numbers"), "{message}");

    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "none",
        "--timeout",
        "0",
        "-o",
        "-",
    ]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains("timeout must be 1..300000"), "{message}");
    assert!(!message.contains("concurrency"), "{message}");

    // Values that start with -- are reachable with the --flag=value form.
    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "xyz",
        "--tile-url",
        "http://127.0.0.1:1/{z}/{x}/{y}.png",
        "--attribution=--not-a-flag",
        "-o",
        "-",
    ]);
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(!message.contains("missing value"), "{message}");
    assert!(message.contains("XYZ basemap failed"), "{message}");
}

#[test]
fn cli_write_failures_report_the_path_and_leave_no_temporaries() {
    let directory = std::env::temp_dir().join(format!("mapshoot-atomic-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let destination = directory.join("map.png");
    std::fs::create_dir_all(&destination).unwrap();
    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "none",
        "-o",
        destination.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains(destination.to_str().unwrap()), "{message}");
    assert!(destination.is_dir());
    let entries: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        entries,
        [std::ffi::OsString::from("map.png")],
        "{entries:?}"
    );

    let missing = directory.join("missing").join("map.png");
    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "none",
        "-o",
        missing.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains(missing.to_str().unwrap()), "{message}");
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn tile_size_512_is_stitched() {
    let server = Fixture::with_body(200, Duration::ZERO, Body::Png(512));
    let mut opts = server.options();
    opts.tile_size = 512;
    let image = render(&opts).unwrap();
    let view = Viewport::new(&opts).unwrap();
    assert_eq!(view.zoom, 0);
    assert_eq!(rgb(&image, 10, 10), [0, 0, 50]);
    assert_eq!(*server.log.paths.lock().unwrap(), ["/0/0/0.png"]);
}

#[test]
fn oversized_tiles_are_rejected() {
    let server = Fixture::with_body(200, Duration::ZERO, Body::Oversize);
    let error = render(&server.options()).unwrap_err().to_string();
    assert!(error.contains("8 MiB"), "{error}");
}

#[test]
fn geojson_nesting_depth_is_limited() {
    let mut value = json!({"type": "Point", "coordinates": [0, 0]});
    for _ in 0..70 {
        value = json!({"type": "GeometryCollection", "geometries": [value]});
    }
    let mut opts = options();
    opts.geojson = Some(value);
    let error = render(&opts).unwrap_err().to_string();
    assert!(error.contains("nesting"), "{error}");
}

#[test]
fn max_zoom_caps_the_selected_zoom() {
    let mut opts = options();
    opts.max_zoom = 3;
    opts.bounds = Bounds::from_center(13.405, 52.52, 1000.0).unwrap();
    assert_eq!(Viewport::new(&opts).unwrap().zoom, 3);
    opts.max_zoom = 20;
    assert!(Viewport::new(&opts).unwrap().zoom > 3);
}

#[test]
fn antimeridian_crossing_views_wrap_tiles_and_overlays() {
    let server = Fixture::new(200, Duration::ZERO, false);
    let mut opts = server.options();
    opts.bounds = Bounds::new(170.0, -5.0, -170.0, 5.0).unwrap();
    opts.geojson = Some(json!({
        "type": "MultiPoint",
        "coordinates": [[179.5, 0.0], [-179.5, 0.0]]
    }));
    opts.style = Style {
        color: "#ff0000".into(),
        fill_opacity: 1.0,
        weight: 0.0,
        ..Style::default()
    };
    let view = Viewport::new(&opts).unwrap();
    assert_eq!(view.zoom, 6);
    let east = view.point(179.5, 0.0, 1);
    let west = view.point(-179.5, 0.0, 1);
    assert!((east.0 - 243.2).abs() < 1.0, "{east:?}");
    assert!((west.0 - 268.8).abs() < 1.0, "{west:?}");
    let image = render(&opts).unwrap();
    assert_eq!(rgb(&image, east.0 as u32, east.1 as u32), [255, 0, 0]);
    assert_eq!(rgb(&image, west.0 as u32, west.1 as u32), [255, 0, 0]);
    let paths = server.log.paths.lock().unwrap().clone();
    assert!(paths.iter().any(|p| p.starts_with("/6/63/")), "{paths:?}");
    assert!(paths.iter().any(|p| p.starts_with("/6/0/")), "{paths:?}");
}

#[test]
fn antimeridian_segments_take_the_shortest_path() {
    let mut opts = options();
    opts.width = 512;
    opts.height = Some(512);
    opts.max_zoom = 1;
    opts.bounds = Bounds::new(-180.0, -85.0, 180.0, 85.0).unwrap();
    opts.geojson = Some(json!({"type": "LineString", "coordinates": [[170, 0], [-170, 0]]}));
    opts.style = Style {
        color: "#ff0000".into(),
        weight: 4.0,
        ..Style::default()
    };
    let view = Viewport::new(&opts).unwrap();
    assert_eq!(view.zoom, 1);
    let image = render(&opts).unwrap();
    // 170E to 170W is 20 degrees across the dateline, not 340 degrees across
    // the map: the line appears at both world edges and not in the middle.
    assert_eq!(rgb(&image, 5, 256), [255, 0, 0]);
    assert_eq!(rgb(&image, 506, 256), [255, 0, 0]);
    assert_eq!(rgb(&image, 256, 256), [221, 221, 221]);
}

#[test]
fn antimeridian_polygons_stay_in_one_piece() {
    let mut opts = options();
    opts.width = 512;
    opts.height = Some(512);
    opts.bounds = Bounds::new(170.0, -5.0, -170.0, 5.0).unwrap();
    opts.geojson = Some(json!({"type": "Polygon", "coordinates": [
        [[179, -1], [-179, -1], [-179, 1], [179, 1], [179, -1]]
    ]}));
    opts.style = Style {
        color: "#0000ff".into(),
        fill_opacity: 1.0,
        weight: 0.0,
        ..Style::default()
    };
    let center = Viewport::new(&opts).unwrap().point(180.0, 0.0, 1);
    let image = render(&opts).unwrap();
    assert_eq!(rgb(&image, center.0 as u32, center.1 as u32), [0, 0, 255]);
    assert_eq!(rgb(&image, 50, 256), [221, 221, 221]);
}

#[test]
fn polar_views_clip_to_the_mercator_edge() {
    let mut opts = options();
    opts.bounds = Bounds::new(-10.0, 80.0, 10.0, MAX_LATITUDE).unwrap();
    // A frame narrower than the box leaves background above the world edge.
    opts.height = Some(600);
    opts.geojson = Some(json!({"type": "Point", "coordinates": [0.0, 89.5]}));
    opts.style = Style {
        color: "#ff0000".into(),
        fill_opacity: 1.0,
        weight: 0.0,
        ..Style::default()
    };
    let view = Viewport::new(&opts).unwrap();
    // Every latitude at or beyond the Web Mercator limit maps to the world top.
    assert_eq!(view.point(0.0, 89.5, 1).1, view.point(0.0, 90.0, 1).1);
    let image = render(&opts).unwrap();
    let point = view.point(0.0, 89.5, 1);
    assert!(point.1 > 0.0 && point.1 < 128.0, "{point:?}");
    assert_eq!(rgb(&image, point.0 as u32, point.1 as u32), [255, 0, 0]);
    // The strip above the world edge keeps the background color.
    assert_eq!(rgb(&image, 128, 2), [221, 221, 221]);
}

#[test]
fn cli_accepts_antimeridian_and_polar_requests() {
    let path = std::env::temp_dir().join(format!("mapshoot-wrap-{}.png", std::process::id()));
    for (center, radius) in [
        ("179.99,0", "10000"),
        ("-179.99,10", "10000"),
        ("0,89", "10000"),
        ("0,-89.9", "50000"),
    ] {
        let result = cli(&[
            "--center",
            center,
            "--radius",
            radius,
            "--basemap",
            "none",
            "--width",
            "320",
            "--height",
            "320",
            "-o",
            path.to_str().unwrap(),
        ]);
        assert!(
            result.status.success(),
            "{center}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cli_buffer_and_aspect_ratio() {
    let dir = std::env::temp_dir();
    let file = dir.join(format!("mapshoot-cli-frame-{}.png", std::process::id()));

    // The ratio fixes the frame and the box is fitted inside it.
    let result = cli(&[
        "--bbox",
        "0,0,1,1",
        "--aspect-ratio",
        "16:9",
        "--width",
        "400",
        "--basemap",
        "none",
        "-o",
        file.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let image = Pixmap::decode_png(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!((image.width(), image.height()), (400, 225));

    // With only a height, the width follows from the ratio.
    let result = cli(&[
        "--bbox",
        "0,0,1,1",
        "--aspect-ratio",
        "2",
        "--height",
        "200",
        "--basemap",
        "none",
        "-o",
        file.to_str().unwrap(),
    ]);
    assert!(result.status.success());
    let image = Pixmap::decode_png(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!((image.width(), image.height()), (400, 200));

    // A buffer grows the area before the height is derived from it.
    let result = cli(&[
        "--bbox",
        "0,0,2,1",
        "--buffer",
        "50000",
        "--width",
        "400",
        "--basemap",
        "none",
        "-o",
        file.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let image = Pixmap::decode_png(&std::fs::read(&file).unwrap()).unwrap();
    let mut opts = options();
    opts.bounds = Bounds::new(0.0, 0.0, 2.0, 1.0)
        .unwrap()
        .buffered(50_000.0)
        .unwrap();
    opts.width = 400;
    opts.height = None;
    let view = Viewport::new(&opts).unwrap();
    assert_eq!(image.width(), view.width);
    assert_eq!(image.height(), view.height);
    assert_ne!(image.height(), 200, "the buffer must change the frame");
    std::fs::remove_file(&file).unwrap();

    let bad_ratios: [&[&str]; 4] = [
        &[
            "--bbox",
            "0,0,1,1",
            "--width",
            "400",
            "--height",
            "300",
            "--aspect-ratio",
            "2",
            "-o",
            "-",
        ],
        &["--bbox", "0,0,1,1", "--aspect-ratio", "0", "-o", "-"],
        &["--bbox", "0,0,1,1", "--aspect-ratio", "1:0", "-o", "-"],
        &["--bbox", "0,0,1,1", "--aspect-ratio", "wide", "-o", "-"],
    ];
    for args in bad_ratios {
        let result = cli(&[args, &["--basemap", "none"]].concat());
        assert!(!result.status.success(), "{args:?}");
        let message = String::from_utf8_lossy(&result.stderr);
        assert!(message.contains("--aspect-ratio"), "{message}");
    }
    for value in ["-1", "1000001", "far"] {
        let result = cli(&[
            "--bbox",
            "0,0,1,1",
            "--buffer",
            value,
            "--basemap",
            "none",
            "-o",
            "-",
        ]);
        assert!(!result.status.success(), "{value}");
        let message = String::from_utf8_lossy(&result.stderr);
        assert!(message.contains("--buffer"), "{message}");
    }
}

#[test]
fn errors_expose_machine_readable_kinds() {
    let mut opts = options();
    opts.style.color = "red".into();
    assert_eq!(render(&opts).unwrap_err().kind(), ErrorKind::InvalidInput);

    let server = Fixture::new(503, Duration::ZERO, false);
    assert_eq!(
        render(&server.options()).unwrap_err().kind(),
        ErrorKind::Http(503)
    );

    let server = Fixture::new(200, Duration::ZERO, true);
    assert_eq!(
        render(&server.options()).unwrap_err().kind(),
        ErrorKind::Other
    );

    let server = Fixture::new(200, Duration::from_millis(200), false);
    let mut opts = server.options();
    opts.timeout = Duration::from_millis(50);
    assert_eq!(render(&opts).unwrap_err().kind(), ErrorKind::Timeout);

    let mut opts = options();
    opts.basemap = Basemap::Xyz {
        url: "http://127.0.0.1:1/{z}/{x}/{y}.png".into(),
        attribution: "Test".into(),
    };
    assert_eq!(render(&opts).unwrap_err().kind(), ErrorKind::Network);
}

#[test]
fn debug_output_redacts_credentials() {
    let mut opts = options();
    opts.basemap = Basemap::Here {
        api_key: "SECRET-KEY".into(),
        style: "lite.day".into(),
    };
    let debug = format!("{opts:?}");
    assert!(!debug.contains("SECRET-KEY"), "{debug}");
    assert!(debug.contains("lite.day"), "{debug}");
    opts.basemap = Basemap::Carto {
        api_key: "SECRET-KEY".into(),
        style: "voyager".into(),
    };
    let debug = format!("{opts:?}");
    assert!(!debug.contains("SECRET-KEY"), "{debug}");
    assert!(debug.contains("voyager"), "{debug}");
    opts.basemap = Basemap::Xyz {
        url: "https://tiles.example/SECRET-KEY/{z}/{x}/{y}.png".into(),
        attribution: "Test".into(),
    };
    let debug = format!("{opts:?}");
    assert!(!debug.contains("SECRET-KEY"), "{debug}");
    assert!(debug.contains("Test"), "{debug}");
}

#[test]
fn renderer_is_reusable_after_errors() {
    let renderer = Renderer::default();
    let mut invalid = options();
    invalid.geojson = Some(json!({"type":"Bogus"}));
    assert!(renderer.render(&invalid).is_err());
    assert!(renderer.render(&options()).is_ok());
}

fn cache_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("png") {
                found.push(path);
            }
        }
    }
    found
}

fn cache_test_dir(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "mapshoot-render-cache-{}-{name}-{nanos}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

#[test]
fn renderer_reuses_cached_tiles_and_replaces_corrupt_entries() {
    let server = Fixture::new(200, Duration::ZERO, false);
    let dir = cache_test_dir("hits");
    let opts = server.options();
    let cache = Cache::at(&dir, Duration::from_secs(3600), 16 * 1024 * 1024).unwrap();
    let first = Renderer::default().with_cache(cache).render(&opts).unwrap();
    let requests = server.log.paths.lock().unwrap().len();
    assert_eq!(requests, 4);

    let cache = Cache::at(&dir, Duration::from_secs(3600), 16 * 1024 * 1024).unwrap();
    let second = Renderer::default().with_cache(cache).render(&opts).unwrap();
    assert_eq!(server.log.paths.lock().unwrap().len(), requests);
    assert_eq!(first.data(), second.data());

    let files = cache_files(&dir);
    assert_eq!(files.len(), 4);
    for file in &files {
        std::fs::write(file, b"not a PNG").unwrap();
    }
    let cache = Cache::at(&dir, Duration::from_secs(3600), 16 * 1024 * 1024).unwrap();
    let third = Renderer::default().with_cache(cache).render(&opts).unwrap();
    assert_eq!(server.log.paths.lock().unwrap().len(), requests * 2);
    assert_eq!(first.data(), third.data());

    // The refetched tiles replaced the corrupt entries.
    let cache = Cache::at(&dir, Duration::from_secs(3600), 16 * 1024 * 1024).unwrap();
    Renderer::default().with_cache(cache).render(&opts).unwrap();
    assert_eq!(server.log.paths.lock().unwrap().len(), requests * 2);

    // Cache hits need no network, even when the download deadline is tiny.
    drop(server);
    let mut offline = opts;
    offline.timeout = Duration::from_millis(1);
    let cache = Cache::at(&dir, Duration::from_secs(3600), 16 * 1024 * 1024).unwrap();
    let cached = Renderer::default()
        .with_cache(cache)
        .render(&offline)
        .unwrap();
    assert_eq!(first.data(), cached.data());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cli_writes_png_and_preserves_existing_file_on_render_failure() {
    let path = std::env::temp_dir().join(format!("mapshoot-cli-{}.png", std::process::id()));
    let path_string = path.to_str().unwrap();
    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "none",
        "--geojson",
        "examples/berlin.geojson",
        "-o",
        path_string,
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let before = std::fs::read(&path).unwrap();
    assert!(Pixmap::decode_png(&before).is_ok());
    let result = cli(&[
        "--bbox",
        "13.37,52.50,13.44,52.54",
        "--basemap",
        "none",
        "--geojson",
        "Cargo.toml",
        "-o",
        path_string,
    ]);
    assert!(!result.status.success());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::remove_file(path).unwrap();
}
