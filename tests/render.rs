use mapshoot::{
    Basemap, Bounds, ErrorKind, MAX_LATITUDE, Options, Renderer, Style, Viewport, render,
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
    options.height = 256;
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
        [0.0, 0.0, 181.0, 1.0],
        [180.0, 0.0, -180.0, 1.0],
        [0.0, 0.0, f64::NAN, 1.0],
    ] {
        assert!(Bounds::new(b[0], b[1], b[2], b[3]).is_err(), "{b:?}");
    }
    // Crossing the antimeridian and reaching a pole are ordinary requests.
    let crossing = Bounds::new(179.0, -1.0, -179.0, 1.0).unwrap();
    assert!(crossing.west > crossing.east);
    let whole = Bounds::new(-180.0, -1.0, 180.0, 1.0).unwrap();
    let mut whole_opts = options();
    whole_opts.bounds = whole;
    whole_opts.width = 512;
    whole_opts.height = 512;
    whole_opts.padding = 0;
    let whole_view = Viewport::new(&whole_opts).unwrap();
    assert_eq!((whole_view.zoom, whole_view.world_size), (1, 512.0));
    assert!(Bounds::new(0.0, 0.0, 1.0, 86.0).is_ok());
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
    assert!(
        polar.north > MAX_LATITUDE && polar.north <= 90.0,
        "{polar:?}"
    );
    let cap = Bounds::from_center(0.0, 85.0, 1_000_000.0).unwrap();
    assert_eq!((cap.west, cap.east), (-180.0, 180.0));
    let opts = options();
    let view = Viewport::new(&opts).unwrap();
    let center = view.point(0.0, 0.0, 1);
    assert!((center.0 - 128.0).abs() <= 1.0 && (center.1 - 128.0).abs() <= 1.0);
    let nw = view.point(opts.bounds.west, opts.bounds.north, 1);
    let se = view.point(opts.bounds.east, opts.bounds.south, 1);
    assert!(nw.0 >= 19.0 && nw.1 >= 19.0 && se.0 <= 237.0 && se.1 <= 237.0);
    assert!(view.point(0.0, 0.005, 1).1 < center.1);
}

#[test]
fn invalid_options_are_errors() {
    let mut cases = Vec::new();
    let mut o = options();
    o.width = 0;
    cases.push(o);
    let mut o = options();
    o.height = 8193;
    cases.push(o);
    let mut o = options();
    o.pixel_ratio = 0;
    cases.push(o);
    let mut o = options();
    o.width = 8192;
    o.height = 8192;
    o.pixel_ratio = 4;
    cases.push(o);
    let mut o = options();
    o.padding = 128;
    cases.push(o);
    let mut o = options();
    o.padding = u32::MAX;
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
    let mut opts = options();
    opts.bounds = Bounds::new(-180.0, -MAX_LATITUDE, 180.0, MAX_LATITUDE).unwrap();
    opts.width = 100;
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
        opts.height = 512;
        opts.padding = 0;
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
    opts.height = 300;
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
fn world_wrap_and_polar_background_do_not_request_invalid_tiles() {
    let server = Fixture::new(200, Duration::ZERO, false);
    let mut opts = server.options();
    opts.width = 512;
    opts.height = 512;
    opts.max_zoom = 0;
    let image = render(&opts).unwrap();
    assert_eq!(rgb(&image, 10, 256), [0, 0, 50]);
    assert_eq!(rgb(&image, 500, 256), [0, 0, 50]);
    assert_eq!(rgb(&image, 256, 10), [221, 221, 221]);
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

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mapshoot"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn cli_help_validation_and_png_stdout() {
    let help = cli(&["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("longitude,latitude"));
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
        "examples/overlay.geojson",
        "--style",
        "examples/style.json",
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
fn cli_missing_here_key_fails_instead_of_producing_a_blank_map() {
    let result = Command::new(env!("CARGO_BIN_EXE_mapshoot"))
        .env_remove("HERE_API_KEY")
        .args(["--bbox", "13.37,52.50,13.44,52.54", "-o", "-"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains("HERE_API_KEY is required"));
    assert!(message.contains("--basemap xyz"));
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
    assert_eq!(view.zoom, 5);
    let east = view.point(179.5, 0.0, 1);
    let west = view.point(-179.5, 0.0, 1);
    assert!((east.0 - 245.0).abs() < 2.0, "{east:?}");
    assert!((west.0 - 267.0).abs() < 2.0, "{west:?}");
    let image = render(&opts).unwrap();
    assert_eq!(rgb(&image, east.0 as u32, east.1 as u32), [255, 0, 0]);
    assert_eq!(rgb(&image, west.0 as u32, west.1 as u32), [255, 0, 0]);
    let paths = server.log.paths.lock().unwrap().clone();
    assert!(paths.iter().any(|p| p.starts_with("/5/31/")), "{paths:?}");
    assert!(paths.iter().any(|p| p.starts_with("/5/0/")), "{paths:?}");
}

#[test]
fn antimeridian_segments_take_the_shortest_path() {
    let mut opts = options();
    opts.width = 512;
    opts.height = 512;
    opts.padding = 0;
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
    opts.height = 512;
    opts.padding = 0;
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
    opts.bounds = Bounds::new(-10.0, 80.0, 10.0, 89.0).unwrap();
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
        "examples/overlay.geojson",
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
