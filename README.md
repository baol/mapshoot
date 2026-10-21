# mapshoot

Native map images from HERE/XYZ PNG tiles and GeoJSON overlays. Rust CLI and
library, CPU rendering with tiny-skia. No browser, Node.js, GUI, GPU, or async
runtime. The executable does not download code or assets at startup.

## Build

```sh
cargo build --release --locked
./target/release/mapshoot --help
# Or install the executable in ~/.cargo/bin:
cargo install --path . --locked
```

Rust and a platform C toolchain are build-time requirements; neither is needed
to run the resulting binary. Five direct crates: tiny-skia (including PNG),
ureq (HTTP/TLS), serde, serde_json, and font8x8 (attribution).
**tiny-skia is pure Rust**, not a C++ Skia binding. HTTPS uses rustls with ring;
ring includes C/assembly compiled into the binary. There is no C++ Skia build,
OpenSSL installation, or system font dependency.

Build for each target OS/architecture; a macOS binary is not a Linux binary.
Linux builds normally use the platform libc; use an appropriate musl target
if a fully static Linux executable is required.

## CLI

For a complete map, use HERE (the default) with a valid `HERE_API_KEY`, or
XYZ with a provider URL. **`--basemap none` deliberately produces no basemap**;
it is only for offline overlay rendering and emits a warning on stderr.

```sh
export HERE_API_KEY='your-key'
mapshoot --center 13.405,52.52 --radius 1500 \
  --here-style explore.day \
  --geojson examples/overlay.geojson --style examples/style.json \
  --output berlin.png

mapshoot --bbox 13.37,52.50,13.44,52.54 \
  --basemap xyz \
  --tile-url 'https://tile.openstreetmap.org/{z}/{x}/{y}.png' \
  --attribution '(c) OpenStreetMap contributors' \
  --output berlin.png

# Offline, with no tile requests:
mapshoot --bbox 13.37,52.50,13.44,52.54 --basemap none \
  --geojson examples/overlay.geojson --output overlay.png

# Binary PNG on stdout; status/errors go to stderr:
mapshoot --center 13.405,52.52 --radius 1500 --basemap none -o - > map.png
```

Coordinates always use **longitude, latitude**; bbox uses **west, south, east,
north**. Radius is in meters, using a spherical Earth bounding box. Bounds and
GeoJSON coordinates must be within Web Mercator latitude limits
(+/-85.05112878 degrees). Pole/antimeridian-crossing areas are rejected; lines
crossing the antimeridian are not unwrapped. Radius must be positive and at most
1,000 km.

The renderer chooses the highest integer zoom that fits the requested area,
centers it, and crops the tiles to the output. Aspect ratio, padding, integer
zoom and pixel rounding can show additional surroundings. Very large areas
that cannot fit even at zoom 0 are rejected instead of cropped.
Horizontal tile coverage wraps at the world edge; regions beyond the north or
south of the Web Mercator world retain the background color.

| Option | Default / meaning |
|---|---|
| `--basemap` | `here`, `xyz`, or `none`; default `here` |
| `--here-style` | `explore.day` |
| `--width`, `--height` | 1024 x 768 logical pixels |
| `--pixel-ratio` | Integer 1..4; scales dimensions and overlays, not tile detail |
| `--padding` | 20 logical pixels |
| `--max-zoom` | 20; allowed 0..22, set to provider's supported maximum |
| `--tile-size` | 256; accepts 256 or 512 and is sent to HERE as the tile `size`, must match returned tile dimensions |
| `--concurrency` | 8 parallel tile requests; allowed 1..32 |
| `--timeout` | 15000 ms total deadline for the tile download phase |
| `--output`, `-o` | Required `.png` path or `-` for stdout |

Images are limited to 8192 logical pixels per side and 64 megapixels total.
At most 2048 tile placements per image, 8 MiB per downloaded tile. Every tile
must be a valid PNG of the configured size. Errors fail the entire render;
there is no silent partial-map fallback. HTTP error messages identify tile
coordinates, not URLs, to avoid leaking API keys. A tile error may wait for
other in-flight requests to finish within the deadline.
Basemap failures exit with status 1, identify HERE or XYZ, and include actionable
HTTP status hints. No image is written on download failure; existing files are
preserved. Missing HERE credentials are an error, not an offline fallback.

The output directory must exist. Rendering and PNG encoding complete before the
output file is opened, and files are written to a temporary file and renamed
into place, so neither a failed render nor a failed write truncates an existing
image. Errors name the offending option or file. Options accept `--flag VALUE`
or `--flag=VALUE`; the latter is required for values that start with `--`.
The timeout covers network work, not JSON parsing, rendering or PNG encoding.
Closing stdout early (`mapshoot ... -o - | head`) is not an error.

### HERE styles and attribution

`explore.day`, `explore.night`, `explore.satellite.day`, `lite.day`, `lite.night`,
`lite.satellite.day`, `logistics.day`, `logistics.night`,
`logistics.satellite.day`, `satellite.day`, `topo.day`, `topo.night`, `dem`.

HERE uses the supplied `maps.hereapi.com/v3/base/mc/{z}/{x}/{y}/png` templates
with `style` and `size` query parameters.
A valid key and appropriate account permissions are required. Tests use local
tiles; live HERE requests are not part of automated validation.

All requests carry a `mapshoot/<version>` `User-Agent`. Providers such as
OpenStreetMap require an identifiable agent; library users can pass a custom
`ureq::Agent` to `Renderer::with_agent` to identify their own application.

HERE images get a visible `(c) HERE` footer. XYZ requires `--attribution`, drawn
above overlays at the bottom right. Attribution is plain printable ASCII, not
HTML; use `(c)` for copyright. Text that cannot fit is an error, never silently
clipped. Observe provider terms and supply any additional required notices.

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
            api_key: std::env::var("HERE_API_KEY")?,
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

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Tests check projection, actual overlay pixels, polygon holes, alpha blending,
GeoJSON validation, PNG round trips, tile placement/cropping, concurrency,
deadlines, redacted errors, attribution and CLI behavior. Local HTTP fixtures
avoid provider credentials and external tile services. No persistent cache or
automatic retries: each render fetches its required tiles.
