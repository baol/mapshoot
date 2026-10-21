# mapshoot

Native map images from HERE, OpenStreetMap, CARTO, or XYZ PNG tiles and GeoJSON
overlays. Rust CLI and library, CPU rendering with tiny-skia. No browser,
Node.js, GUI, GPU, or async runtime. The executable does not download code or
assets at startup.

## Berlin from GeoJSON

![Berlin Mitte in four basemap styles with the same GeoJSON overlay](docs/berlin-basemaps.png)

mapshoot is built for **automatic, reproducible screenshots of GeoJSON data**:
give it a center and radius (or a bbox), a basemap, and a GeoJSON file, and it
writes the finished PNG. No browser, screenshot service, manual tile stitching,
or post-processing step is involved. The image above renders the same overlay
data (`examples/berlin.geojson`) over four basemaps.

```sh
# HERE, with the bundled Berlin overlay:
export MAPSHOOT_HERE_API_KEY='your-key'   # legacy: HERE_API_KEY
mapshoot --center 13.405,52.52 --radius 2600 --width 800 --height 450 \
  --basemap here --here-style explore.day \
  --geojson examples/berlin.geojson --style examples/berlin-style.json \
  --output berlin-here.png

# The same data and framing with OpenStreetMap, no key required:
mapshoot --center 13.405,52.52 --radius 2600 --width 800 --height 450 \
  --basemap osm \
  --geojson examples/berlin.geojson --style examples/berlin-style.json \
  --output berlin-osm.png
```

Swap `--basemap` and the style flag to compare providers; the basemap, style,
and key tables are below. The overlay is drawn in the same projected coordinate
space as the tiles, so points, lines, and polygons line up at every zoom and
fractional scale.

## Build

```sh
cargo build --release --locked
./target/release/mapshoot --help
# Or install the executable in ~/.cargo/bin:
cargo install --path . --locked
```

Rust and a platform C toolchain are build-time requirements; neither is needed
to run the resulting binary. Rust 1.85 or newer is required (2024 edition).
Five direct crates: tiny-skia (including PNG),
ureq (HTTP/TLS), serde, serde_json, and font8x8 (attribution).
**tiny-skia is pure Rust**, not a C++ Skia binding. HTTPS uses rustls with ring;
ring includes C/assembly compiled into the binary. There is no C++ Skia build,
OpenSSL installation, or system font dependency.

Build for each target OS/architecture; a macOS binary is not a Linux binary.
Linux builds normally use the platform libc; use an appropriate musl target
if a fully static Linux executable is required.

## CLI

For a complete map, use HERE (the default), OSM, CARTO, or XYZ with a provider
URL. HERE and CARTO need an API key; OSM does not.
**`--basemap none` deliberately produces no basemap**; it is only for offline
overlay rendering and emits a warning on stderr.

```sh
export MAPSHOOT_HERE_API_KEY='your-key'   # legacy: HERE_API_KEY
mapshoot --center 13.405,52.52 --radius 1500 \
  --here-style explore.day \
  --geojson examples/overlay.geojson --style examples/style.json \
  --output berlin.png

# OpenStreetMap, no key:
mapshoot --bbox 13.37,52.50,13.44,52.54 --basemap osm \
  --output berlin-osm.png

# CARTO, free key from https://carto.com/basemaps/apikey:
export MAPSHOOT_CARTO_API_KEY='your-key'  # legacy: CARTO_API_KEY
mapshoot --bbox 13.37,52.50,13.44,52.54 --basemap carto \
  --carto-style voyager --output berlin-carto.png

# Any XYZ provider:
mapshoot --bbox 13.37,52.50,13.44,52.54 \
  --basemap xyz \
  --tile-url 'https://tiles.example.com/{z}/{x}/{y}.png' \
  --attribution '(c) Example Maps' \
  --output berlin.png

# Offline, with no tile requests:
mapshoot --bbox 13.37,52.50,13.44,52.54 --basemap none \
  --geojson examples/overlay.geojson --output overlay.png

# A 16:9 frame with 1 km of map around the box (the box is fitted inside):
mapshoot --bbox 13.37,52.50,13.44,52.54 --buffer 1000 \
  --aspect-ratio 16:9 --width 1280 --basemap none --output frame.png

# Binary PNG on stdout; status/errors go to stderr:
mapshoot --center 13.405,52.52 --radius 1500 --basemap none -o - > map.png
```

### Basemaps and keys

| `--basemap` | Provider | Key variables (first non-empty) | Style flag |
|---|---|---|---|
| `here` (default) | HERE | `MAPSHOOT_HERE_API_KEY`, then `HERE_API_KEY` | `--here-style`, default `explore.day` |
| `osm` | OpenStreetMap | none | none |
| `carto` | CARTO | `MAPSHOOT_CARTO_API_KEY`, then `CARTO_API_KEY` | `--carto-style`, default `voyager` |
| `xyz` | Any XYZ provider | credentials belong in `--tile-url` | none |
| `none` | No tiles | none | none |

The `MAPSHOOT_*` variable takes precedence over the unprefixed legacy name.
If neither variable is set, or both are empty, the key is an error rather than
a silent fallback. The `osm` and `carto` presets serve 256 px tiles; use
`--tile-size 512` only with `here` or `xyz`.

Coordinates always use **longitude, latitude**; bbox uses **west, south, east,
north**. Radius is in meters, using a spherical Earth bounding box. A bbox with
`west > east` crosses the antimeridian, and `--center` with `--radius` wraps
automatically. Bbox longitudes must be within -180..180 and latitudes within
-85.05112878..85.05112878, so the whole world is spelled
`-180,-85.05112878,180,85.05112878`. Values outside those limits are errors:
the image is the box, so silently clipping it would be wrong. GeoJSON
coordinates accept -90..90 and clamp to the world edge. Line and polygon
segments take the shortest route around the globe, and shapes crossing the
antimeridian are drawn on both sides of the map. Radius must be positive and at
most 1,000 km; `--center` boxes clamp at the Web Mercator limit.

The bbox maps to the image edges. Unless `--height` or `--aspect-ratio` says
otherwise, the height follows from the box aspect ratio in Web Mercator, so
180x90 degrees at `--width 1024` yields a 1024x575 image with no surroundings.
An explicit height or an aspect ratio fixes the frame and fits the box inside
it, centered; a larger frame shows the extra map around the box (which can be
background beyond the world edge). `--buffer` grows the box on every side
before the frame is resolved, so the image still covers at least that many
meters. `--padding` was removed: with exact framing there is no fit padding.

Tiles are drawn at a fractional scale: the renderer picks the smallest zoom
whose world is at least as large as the image and downsamples it with bicubic
resampling, or nearest filtering at whole-tile scales. Overlays are projected
with the same continuous scale and stay vector-crisp. Horizontal tile coverage
wraps at the world edge.

| Option | Default / meaning |
|---|---|
| `--basemap` | `here`, `osm`, `carto`, `xyz`, or `none`; default `here` |
| `--here-style` | HERE only; default `explore.day` |
| `--carto-style` | CARTO only; default `voyager` |
| `--tile-url`, `--attribution` | XYZ only; URL template with `{z}`, `{x}`, `{y}`, and required plain-ASCII attribution |
| `--buffer` | 0 meters; grows the box on every side, wrapping longitudes and clamping at the Mercator limit |
| `--width` | 1024 logical pixels |
| `--height` | follows the box aspect; an explicit value fits the box inside |
| `--aspect-ratio` | `W:H` or a number; fixes the frame and fits the box inside |
| `--pixel-ratio` | Integer 1..4; scales dimensions and overlays, not tile detail |
| `--max-zoom` | 20; allowed 0..22, with preset limits `osm` 19 and `carto` 20 |
| `--tile-size` | 256; accepts 256 or 512, must match returned tile dimensions; `osm` and `carto` are 256 only |
| `--concurrency` | 8; allowed 1..32, with the `osm` preset defaulting to and limited to 2 |
| `--timeout` | 15000 ms total deadline for the tile download phase |
| `--output`, `-o` | Required `.png` path or `-` for stdout |

Images are limited to 8192 logical pixels per side and 64 megapixels total. A
height derived from the box aspect that falls outside that range is an error
naming the needed height; reduce `--width` or set `--height`/`--aspect-ratio`.
At most 2048 tile placements per image, 8 MiB per downloaded tile. Every tile
must be a valid PNG of the configured size. Errors fail the entire render;
there is no silent partial-map fallback. HTTP error messages identify tile
coordinates, not URLs, to avoid leaking API keys. A tile error may wait for
other in-flight requests to finish within the deadline.
Basemap failures exit with status 1, identify HERE, OSM, CARTO, or XYZ, and
include actionable HTTP status hints. No image is written on download failure;
existing files are preserved. Missing HERE or CARTO credentials are an error,
not an offline fallback.

The output directory must exist. Rendering and PNG encoding complete before the
output file is opened, and files are written to a temporary file and renamed
into place, so neither a failed render nor a failed write truncates an existing
image. Errors name the offending option or file. Options accept `--flag VALUE`
or `--flag=VALUE`; the latter is required for values that start with `--`.
The timeout covers network work, not JSON parsing, rendering or PNG encoding.
Closing stdout early (`mapshoot ... -o - | head`) is not an error.

### HERE styles and attribution

`--here-style` accepts `explore.day`, `explore.night`,
`explore.satellite.day`, `lite.day`, `lite.night`, `lite.satellite.day`,
`logistics.day`, `logistics.night`, `logistics.satellite.day`, `satellite.day`,
`topo.day`, `topo.night`, and `dem`. The default is `explore.day`.

HERE uses the supplied `maps.hereapi.com/v3/base/mc/{z}/{x}/{y}/png` templates
with `style` and `size` query parameters.
A valid key and appropriate account permissions are required. Tests use local
tiles; live HERE requests are not part of automated validation.

### CARTO styles and attribution

`--carto-style` accepts:

- `voyager` (default), `voyager_nolabels`, `voyager_labels_under`
- `light_all` (CARTO Positron), `light_nolabels`
- `dark_all` (CARTO Dark Matter), `dark_nolabels`

CARTO raster tiles come from
`https://basemaps.cartocdn.com/rastertiles/<style>/{z}/{x}/{y}.png?key=...`.
A key is required; request a free one at
<https://carto.com/basemaps/apikey>. CARTO images carry the visible
`(c) OpenStreetMap contributors, (c) CARTO` footer.

### OpenStreetMap

`--basemap osm` uses the official `tile.openstreetmap.org` server and needs no
key or style flag. Images carry the `(c) OpenStreetMap contributors` footer.
The preset defaults to `--max-zoom 19`, the standard layer's maximum, and
`--concurrency 2`; higher values for either option are rejected.

The OSM tile usage policy forbids bulk and offline downloading. mapshoot keeps
no persistent tile cache and fetches only the tiles used by the requested image.
Use the OSM preset for one-off or interactive rendering, not for building tile
archives or high-volume services. For those, self-host or use `--basemap xyz`
with a provider that permits it.

### XYZ

`--basemap xyz` requires `--tile-url` with `{z}`, `{x}`, `{y}` placeholders and
`--attribution`. It has no preset styles and no key variables: put any
credentials in the URL. The URL and attribution must be valid for every tile.

All requests carry a `mapshoot/<version>` `User-Agent`. Providers such as
OpenStreetMap require an identifiable agent; library users can pass a custom
`ureq::Agent` to `Renderer::with_agent` to identify their own application.

HERE, OSM, and CARTO images get a visible attribution footer drawn at the
bottom right. XYZ uses the supplied `--attribution`. Attribution is plain
printable ASCII, not HTML; use `(c)` for copyright. Text that cannot fit is an
error, never silently clipped. Observe provider terms and supply any additional
required notices.

### GeoJSON and styling

Supports Feature, FeatureCollection, GeometryCollection, Point, LineString,
Polygon, and all Multi variants. Null Feature geometries and empty collections
are allowed. Polygon rings must be closed; holes use even-odd filling regardless
of winding direction. Points become circles; lines have round caps and joins.
Rendering is antialiased and clipped to the image.

`--style` accepts a JSON object:

```json
{
  "color": "#e63946",
  "fillColor": "#e63946",
  "weight": 3,
  "opacity": 1,
  "fillOpacity": 0.25,
  "radius": 6
}
```

Colors use `#RRGGBB`; opacity values are 0..1. Width/radius are logical pixels.
Defaults: blue `#3388ff`, weight 3, opacity 1, fillOpacity 0.2, radius 6.
`fillColor` defaults to `color`. Use weight 0 for no outline or fillOpacity 0
for no fill. Unknown style fields are errors to catch misspellings.
A single style applies to all features; feature properties are not interpreted.

## Rust library

```rust
use mapshoot::{Basemap, Bounds, Options, Renderer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut options = Options::new(
        Bounds::from_center(13.405, 52.52, 1500.0)?,
        Basemap::Here {
            api_key: std::env::var("MAPSHOOT_HERE_API_KEY")
                .or_else(|_| std::env::var("HERE_API_KEY"))?,
            style: "explore.day".into(),
        },
    );
    options.geojson = Some(serde_json::from_str(
        r#"{"type":"Point","coordinates":[13.405,52.52]}"#
    )?);
    let renderer = Renderer::default(); // Reuse for pooled HTTP connections.
    let image = renderer.render(&options)?;
    image.save_png("berlin.png")?;
    Ok(())
}
```

`render(&options)` is also available as a one-shot function.
The returned tiny-skia Pixmap can be encoded in memory or further drawn upon.

For OSM use `Basemap::Osm`. For CARTO use
`Basemap::Carto { api_key, style }` with a style from `CARTO_STYLES`; HERE
styles are listed in `HERE_STYLES`. The library itself does not read
environment variables; the example reads the key for its application.

`options.height` is `Option<u32>`. The default `None` follows the bbox aspect
ratio exactly; `Some(h)` fixes the frame and fits the bbox inside it. Use
`Bounds::buffered(meters)` for the CLI's `--buffer`, and `Bounds::aspect()` for
the projected width/height ratio.

Errors are `mapshoot::Error`. Besides the `Display` message they expose
`Error::kind()`: `InvalidInput`, `Http(status)`, `Network`, `Timeout` or
`Other`, so callers can decide whether a retry may help. `Debug` for `Basemap`
and `Options` redacts API keys and tile URLs, so options can be logged safely.

## Migration from the browser prototype

This replaces the Node/Leaflet/Playwright implementation, not a second backend.
PNG raster providers, HERE styles, bbox/center/radius, GeoJSON and the basic
style-file format are retained. Node APIs, `configureMap`, Leaflet plugins,
JavaScript hooks, HTML attribution and JPEG output are intentionally removed.
Fractional pixel ratios are no longer accepted.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.

The embedded 8x8 bitmap font comes from the `font8x8` crate, distributed under
the MIT license.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

The same three commands run in CI on stable Rust (`.github/workflows/ci.yml`).

Tests check projection, exact bbox framing, fractional tile resampling, actual
overlay pixels, polygon holes, alpha blending, GeoJSON validation, PNG round
trips, tile placement/cropping, concurrency, deadlines, redacted errors,
attribution and CLI behavior. Local HTTP fixtures avoid provider credentials and
external tile services. No persistent cache or automatic retries: each render
fetches its required tiles.
