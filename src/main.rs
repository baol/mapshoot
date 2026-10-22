#![forbid(unsafe_code)]
use mapshoot::{Basemap, Bounds, Cache, DEFAULT_MAX_BYTES, DEFAULT_TTL, Options, Renderer, Style};
use std::{
    collections::BTreeMap,
    io::{ErrorKind, Write},
    path::Path,
    time::Duration,
};

const HELP: &str = "\
Usage: mapshoot --bbox west,south,east,north --output map.png [options]
       mapshoot --center longitude,latitude --radius meters --output map.png
       mapshoot --geojson file.geojson --output map.png [options]

Basemap:
  --basemap here|osm|carto|xyz|none
                            Default: osm (no key); none disables map tiles
  --here-style NAME         HERE only (default: explore.day)
  --carto-style NAME        CARTO only (default: voyager)
  --tile-url URL            XYZ only: HTTP(S) PNG template with {z}, {x}, {y}
  --attribution TEXT        XYZ only: required, printable ASCII, not HTML
  --tile-size N             256 or 512 (default: 256); osm/carto are 256 only
  HERE key:  MAPSHOOT_HERE_API_KEY  (preferred; legacy HERE_API_KEY)
  CARTO key: MAPSHOOT_CARTO_API_KEY (preferred; legacy CARTO_API_KEY)
             free key at https://carto.com/basemaps/apikey
  OSM: no key; defaults to --max-zoom 19 and --concurrency 2.
  Avoid bulk/offline downloads. XYZ credentials belong in --tile-url.

  HERE styles: explore.day, explore.night, explore.satellite.day, lite.day,
    lite.night, lite.satellite.day, logistics.day, logistics.night,
    logistics.satellite.day, satellite.day, topo.day, topo.night, dem
  CARTO styles: voyager, voyager_nolabels, voyager_labels_under, light_all,
    light_nolabels, dark_all, dark_nolabels

Area:
  --bbox W,S,E,N            Longitude,latitude; west > east crosses the antimeridian
  --center LON,LAT          Requires --radius; alternative to --bbox
  --radius METERS           Radius around --center, at most 1000000
  --buffer METERS           Expand the box on every side (default: 0)
  --padding METERS          Synonym for --buffer (default: 0)

  --bbox or --center/--radius may be omitted with --geojson; the box then
  follows the GeoJSON extent and includes marker and stroke graphics.

Image:
  --width N                 Image width (default: 1024)
  --height N                Image height (default: follows the box aspect)
  --aspect-ratio W:H|N      Frame shape; the box is fitted inside
  --pixel-ratio N           Integer output scale 1..4 (default: 1)
  --max-zoom N              Highest tile zoom 0..22 (default: 19 for osm, 20 otherwise)
  --concurrency N           Parallel downloads 1..32 (default: 2 for osm, 8 otherwise)
  --timeout N               Total download deadline in ms (default: 15000)

Cache:
  --no-cache                Disable the tile cache
  --cache-dir PATH          Cache root (default: system cache directory)
  --cache-ttl DAYS          Freshness lifetime 1..3650 (default: 7)
  --cache-max-mb MB         Size cap 1..102400 (default: 512)
  MAPSHOOT_CACHE_DIR overrides the system cache directory; empty disables it.

Overlay and output:
  --geojson FILE            GeoJSON overlay; works with any basemap
  --style FILE              JSON: color, fillColor, weight, opacity, fillOpacity, radius
  --output, -o FILE         PNG output; use - for stdout
  --help, -h                Show help
  --version                 Show version

Options accept --flag VALUE or --flag=VALUE.
Radius, buffer, and padding are in meters. PNG output only.
";

fn main() {
    if let Err(err) = run() {
        eprintln!("mapshoot: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut values = BTreeMap::new();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            "--version" => {
                println!("mapshoot {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => {}
        }
        let (flag, inline) = match argument.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_string(), Some(value.to_string()))
            }
            _ => (argument, None),
        };
        let flag = if flag == "-o" {
            "--output".into()
        } else {
            flag
        };
        if ![
            "--bbox",
            "--center",
            "--radius",
            "--output",
            "--basemap",
            "--here-style",
            "--carto-style",
            "--tile-url",
            "--attribution",
            "--geojson",
            "--style",
            "--width",
            "--height",
            "--buffer",
            "--padding",
            "--aspect-ratio",
            "--pixel-ratio",
            "--max-zoom",
            "--tile-size",
            "--concurrency",
            "--timeout",
            "--no-cache",
            "--cache-dir",
            "--cache-ttl",
            "--cache-max-mb",
        ]
        .contains(&flag.as_str())
        {
            return Err(format!("unknown option {flag}; use --help").into());
        }
        if flag == "--no-cache" {
            if inline.is_some() {
                return Err("--no-cache does not take a value".into());
            }
            if values.insert(flag, String::new()).is_some() {
                return Err("duplicate option --no-cache".into());
            }
            continue;
        }
        let value = match inline {
            Some(value) => value,
            None => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for {flag}"))?;
                if value.starts_with("--") {
                    return Err(format!(
                        "missing value for {flag}; use {flag}=VALUE for a value starting with --"
                    )
                    .into());
                }
                value
            }
        };
        if values.insert(flag.clone(), value).is_some() {
            return Err(format!("duplicate option {flag}").into());
        }
    }
    let output = values
        .get("--output")
        .ok_or("--output is required; use --help")?;
    if output.is_empty() {
        return Err("--output must not be empty".into());
    }
    if output != "-" && !output.to_ascii_lowercase().ends_with(".png") {
        return Err("only PNG output is supported; use a .png extension or - for stdout".into());
    }
    let geojson: Option<serde_json::Value> = match values.get("--geojson") {
        Some(file) => Some(read_json("--geojson", file)?),
        None => None,
    };
    let buffer_set = values.contains_key("--buffer");
    let padding_set = values.contains_key("--padding");
    if buffer_set && padding_set {
        return Err("use either --buffer or --padding, not both".into());
    }
    let margin_flag = if buffer_set { "--buffer" } else { "--padding" };
    let margin = if buffer_set || padding_set {
        let value = &values[margin_flag];
        value
            .parse::<f64>()
            .map_err(|_| format!("{margin_flag} must be a number of meters, got {value:?}"))
    } else {
        Ok(0.0)
    }?;
    if !margin.is_finite() || !(0.0..=1_000_000.0).contains(&margin) {
        return Err(format!("{margin_flag} must be 0..1000000 meters").into());
    }
    let (mut bounds, derived) = match (
        values.get("--bbox"),
        values.get("--center"),
        values.get("--radius"),
    ) {
        (Some(bbox), None, None) => {
            let b = numbers::<4>("--bbox", bbox)?;
            (Bounds::new(b[0], b[1], b[2], b[3])?, false)
        }
        (None, Some(center), Some(radius)) => {
            let c = numbers::<2>("--center", center)?;
            let radius = radius
                .parse()
                .map_err(|_| format!("--radius must be a number of meters, got {radius:?}"))?;
            (Bounds::from_center(c[0], c[1], radius)?, false)
        }
        (Some(_), _, _) => {
            return Err("provide either --bbox or --center with --radius, not both".into());
        }
        (None, Some(_), None) => return Err("--center requires --radius".into()),
        (None, None, Some(_)) => return Err("--radius requires --center".into()),
        (None, None, None) => match geojson.as_ref() {
            Some(value) => (
                Bounds::from_geojson(value, margin).map_err(|err| {
                    format!("--geojson: {err}; use --padding, --bbox, or --center/--radius")
                })?,
                true,
            ),
            None => {
                return Err("provide --bbox, --center with --radius, or --geojson".into());
            }
        },
    };
    if !derived {
        bounds = bounds
            .buffered(margin)
            .map_err(|err| format!("{margin_flag}: {err}"))?;
    }
    let basemap_name = values.get("--basemap").map(String::as_str).unwrap_or("osm");
    if !["here", "osm", "carto", "xyz", "none"].contains(&basemap_name) {
        return Err("--basemap must be here, osm, carto, xyz or none".into());
    }
    if basemap_name != "here" && values.contains_key("--here-style") {
        return Err("--here-style requires --basemap here".into());
    }
    if basemap_name != "carto" && values.contains_key("--carto-style") {
        return Err("--carto-style requires --basemap carto".into());
    }
    if basemap_name != "xyz"
        && (values.contains_key("--tile-url") || values.contains_key("--attribution"))
    {
        return Err("--tile-url and --attribution require --basemap xyz".into());
    }
    let basemap = match basemap_name {
        "none" => Basemap::None,
        "here" => Basemap::Here {
            api_key: provider_key("HERE")?,
            style: values
                .get("--here-style")
                .cloned()
                .unwrap_or_else(|| "explore.day".into()),
        },
        "osm" => Basemap::Osm,
        "carto" => Basemap::Carto {
            api_key: provider_key("CARTO")?,
            style: values
                .get("--carto-style")
                .cloned()
                .unwrap_or_else(|| "voyager".into()),
        },
        "xyz" => Basemap::Xyz {
            url: values
                .get("--tile-url")
                .ok_or("--tile-url is required for xyz")?
                .clone(),
            attribution: values
                .get("--attribution")
                .ok_or("--attribution is required for xyz")?
                .clone(),
        },
        _ => return Err("--basemap must be here, osm, carto, xyz or none".into()),
    };
    let mut options = Options::new(bounds, basemap);
    macro_rules! number {
        ($flag:literal, $field:ident) => {
            if let Some(value) = values.get($flag) {
                options.$field = value
                    .parse()
                    .map_err(|_| format!("{} must be a number, got {:?}", $flag, value))?;
            }
        };
    }
    number!("--pixel-ratio", pixel_ratio);
    number!("--max-zoom", max_zoom);
    number!("--tile-size", tile_size);
    number!("--concurrency", concurrency);
    let ratio = values
        .get("--aspect-ratio")
        .map(|v| aspect_ratio(v))
        .transpose()?;
    let parse_dimension = |flag: &str, value: &str| -> Result<u32, Box<dyn std::error::Error>> {
        value
            .parse()
            .map_err(|_| format!("{flag} must be a number, got {value:?}").into())
    };
    let mut width = values
        .get("--width")
        .map(|value| parse_dimension("--width", value))
        .transpose()?;
    let mut height = values
        .get("--height")
        .map(|value| parse_dimension("--height", value))
        .transpose()?;
    if let Some(ratio) = ratio {
        match (width, height) {
            (Some(w), Some(h)) => {
                let expected = ((f64::from(w) / ratio).round() as u32).max(1);
                if h != expected {
                    return Err(format!(
                        "--height {h} does not match --width {w} at the requested --aspect-ratio; omit one of them or use --height {expected}"
                    )
                    .into());
                }
            }
            (Some(w), None) => height = Some(((f64::from(w) / ratio).round() as u32).max(1)),
            (None, Some(h)) => width = Some(((f64::from(h) * ratio).round() as u32).max(1)),
            (None, None) => {
                width = Some(1024);
                height = Some(((1024.0 / ratio).round() as u32).max(1));
            }
        }
    }
    options.width = width.unwrap_or(1024);
    options.height = height;
    if let Some(timeout) = values.get("--timeout") {
        options.timeout = Duration::from_millis(
            timeout
                .parse()
                .map_err(|_| format!("--timeout must be whole milliseconds, got {timeout:?}"))?,
        );
    }
    let no_cache = values.contains_key("--no-cache");
    if no_cache {
        for flag in ["--cache-dir", "--cache-ttl", "--cache-max-mb"] {
            if values.contains_key(flag) {
                return Err(format!("use either --no-cache or {flag}").into());
            }
        }
    }
    let cache_ttl_days = match values.get("--cache-ttl") {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| format!("--cache-ttl must be whole days, got {value:?}"))?,
        None => DEFAULT_TTL.as_secs() / (24 * 60 * 60),
    };
    if !(1..=3650).contains(&cache_ttl_days) {
        return Err("--cache-ttl must be 1..3650 days".into());
    }
    let cache_max_mb = match values.get("--cache-max-mb") {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| format!("--cache-max-mb must be whole megabytes, got {value:?}"))?,
        None => DEFAULT_MAX_BYTES / (1024 * 1024),
    };
    if !(1..=102_400).contains(&cache_max_mb) {
        return Err("--cache-max-mb must be 1..102400 MB".into());
    }
    options.geojson = geojson;
    if let Some(file) = values.get("--style") {
        options.style = read_json::<Style>("--style", file)?;
    }
    if derived {
        options
            .include_geojson_graphics()
            .map_err(|err| format!("--geojson: {err}"))?;
    }
    if matches!(options.basemap, Basemap::None) {
        eprintln!(
            "mapshoot: warning: --basemap none disables map tiles; output contains only overlays on a plain background"
        );
    }
    let cache_ttl = Duration::from_secs(cache_ttl_days * 24 * 60 * 60);
    let cache_max_bytes = cache_max_mb * 1024 * 1024;
    let cache = if no_cache {
        None
    } else if let Some(path) = values.get("--cache-dir") {
        Some(
            Cache::at(path, cache_ttl, cache_max_bytes)
                .map_err(|err| format!("--cache-dir {path:?}: {err}"))?,
        )
    } else {
        Cache::system_with(cache_ttl, cache_max_bytes)
    };
    let mut renderer = Renderer::default();
    if let Some(cache) = cache {
        renderer = renderer.with_cache(cache);
    }
    let image = renderer.render(&options)?;
    let png = image.encode_png()?;
    if output == "-" {
        let mut stdout = std::io::stdout().lock();
        if let Err(err) = stdout.write_all(&png).and_then(|()| stdout.flush()) {
            // The consumer went away (`mapshoot ... | head`): nothing to report.
            if err.kind() == ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(err.into());
        }
    } else {
        write_atomically(output, &png)?;
        eprintln!("Saved {output}");
    }
    Ok(())
}

/// Read a provider key from `MAPSHOOT_<PROVIDER>_API_KEY`, falling back to the
/// legacy unprefixed name. The prefixed variable takes precedence.
fn provider_key(provider: &str) -> Result<String, Box<dyn std::error::Error>> {
    let prefixed = format!("MAPSHOOT_{provider}_API_KEY");
    let legacy = format!("{provider}_API_KEY");
    for name in [&prefixed, &legacy] {
        if let Ok(value) = std::env::var(name) {
            let value = value.trim();
            if !value.is_empty() {
                return Ok(value.to_string());
            }
        }
    }
    let hint = if provider == "CARTO" {
        "; get a free key at https://carto.com/basemaps/apikey"
    } else {
        "; set it or use --basemap osm or --basemap xyz"
    };
    Err(format!(
        "{prefixed} (or {legacy}) is required for the {} basemap{hint}",
        provider.to_ascii_lowercase()
    )
    .into())
}

fn read_json<T: serde::de::DeserializeOwned>(
    flag: &str,
    path: &str,
) -> Result<T, Box<dyn std::error::Error>> {
    let file = std::fs::File::open(path).map_err(|err| format!("{flag} {path}: {err}"))?;
    serde_json::from_reader(file).map_err(|err| format!("{flag} {path}: {err}").into())
}

/// Write next to the destination and rename, so a failed write never leaves a
/// truncated or partially written image behind.
fn write_atomically(path: &str, data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let destination = Path::new(path);
    let name = destination
        .file_name()
        .ok_or("--output must name a file")?
        .to_string_lossy();
    let temporary = destination.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(data)?;
        drop(file);
        std::fs::rename(&temporary, destination)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|err| format!("cannot write {path}: {err}").into())
}

/// Parse `W:H` or a plain number into a positive width-to-height ratio.
fn aspect_ratio(value: &str) -> Result<f64, Box<dyn std::error::Error>> {
    let invalid = || format!("--aspect-ratio must be W:H or a positive number, got {value:?}");
    let ratio = match value.split_once(':') {
        Some((width, height)) => {
            let width: f64 = width.trim().parse().map_err(|_| invalid())?;
            let height: f64 = height.trim().parse().map_err(|_| invalid())?;
            width / height
        }
        None => value.trim().parse().map_err(|_| invalid())?,
    };
    if !ratio.is_finite() || ratio <= 0.0 {
        return Err(invalid().into());
    }
    Ok(ratio)
}

fn numbers<const N: usize>(
    flag: &str,
    value: &str,
) -> Result<[f64; N], Box<dyn std::error::Error>> {
    let values: Vec<f64> = value
        .split(',')
        .map(|part| part.trim().parse())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("{flag} expects {N} comma-separated numbers, got {value:?}"))?;
    values
        .try_into()
        .map_err(|_| format!("{flag} expects {N} comma-separated numbers, got {value:?}").into())
}
