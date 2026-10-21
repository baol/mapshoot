#![forbid(unsafe_code)]
use mapshoot::{Basemap, Bounds, Options, Renderer, Style};
use std::{
    collections::BTreeMap,
    io::{ErrorKind, Write},
    path::Path,
    time::Duration,
};

const HELP: &str = "\
Usage: mapshoot --bbox west,south,east,north --output map.png [options]
       mapshoot --center longitude,latitude --radius meters --output map.png

  --basemap here|xyz|none   Default: here; none explicitly disables the basemap
  --here-style NAME         Default: explore.day
  --tile-url URL            HTTP(S) PNG template with {z}, {x}, {y}
  --attribution TEXT        Required for XYZ; printable ASCII, not HTML
  --geojson FILE            GeoJSON overlay
  --style FILE              JSON: color, fillColor, weight, opacity, fillOpacity, radius
  --width N --height N      Logical dimensions (default: 1024 x 768)
  --pixel-ratio N           Integer output scale 1..4 (default: 1)
  --padding N               Fit padding in logical pixels (default: 20)
  --max-zoom N              Highest tile zoom 0..22 (default: 20)
  --tile-size N             256 or 512, must match provider (default: 256)
  --concurrency N           Parallel downloads 1..32 (default: 8)
  --timeout N               Total download deadline in ms (default: 15000)
  --output, -o FILE         PNG output; use - for stdout
  --help, -h                Show help
  --version                 Show version

Options accept --flag VALUE or --flag=VALUE.
Coordinates are longitude,latitude. Radius is in meters. PNG output only.
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
            "--tile-url",
            "--attribution",
            "--geojson",
            "--style",
            "--width",
            "--height",
            "--pixel-ratio",
            "--padding",
            "--max-zoom",
            "--tile-size",
            "--concurrency",
            "--timeout",
        ]
        .contains(&flag.as_str())
        {
            return Err(format!("unknown option {flag}; use --help").into());
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
    let bounds = match (
        values.get("--bbox"),
        values.get("--center"),
        values.get("--radius"),
    ) {
        (Some(bbox), None, None) => {
            let b = numbers::<4>("--bbox", bbox)?;
            Bounds::new(b[0], b[1], b[2], b[3])?
        }
        (None, Some(center), Some(radius)) => {
            let c = numbers::<2>("--center", center)?;
            let radius = radius
                .parse()
                .map_err(|_| format!("--radius must be a number of meters, got {radius:?}"))?;
            Bounds::from_center(c[0], c[1], radius)?
        }
        _ => return Err("provide either --bbox or --center with --radius".into()),
    };
    let basemap = match values
        .get("--basemap")
        .map(String::as_str)
        .unwrap_or("here")
    {
        "none" => Basemap::None,
        "here" => Basemap::Here {
            api_key: std::env::var("HERE_API_KEY").map_err(|_| {
                "HERE_API_KEY is required for the HERE basemap; set it or use --basemap xyz with --tile-url and --attribution"
            })?,
            style: values
                .get("--here-style")
                .cloned()
                .unwrap_or_else(|| "explore.day".into()),
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
        _ => return Err("--basemap must be here, xyz or none".into()),
    };
    if !matches!(basemap, Basemap::Here { .. }) && values.contains_key("--here-style") {
        return Err("--here-style requires --basemap here".into());
    }
    if !matches!(basemap, Basemap::Xyz { .. })
        && (values.contains_key("--tile-url") || values.contains_key("--attribution"))
    {
        return Err("--tile-url and --attribution require --basemap xyz".into());
    }
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
    number!("--width", width);
    number!("--height", height);
    number!("--padding", padding);
    number!("--pixel-ratio", pixel_ratio);
    number!("--max-zoom", max_zoom);
    number!("--tile-size", tile_size);
    number!("--concurrency", concurrency);
    if let Some(timeout) = values.get("--timeout") {
        options.timeout = Duration::from_millis(
            timeout
                .parse()
                .map_err(|_| format!("--timeout must be whole milliseconds, got {timeout:?}"))?,
        );
    }
    if let Some(file) = values.get("--geojson") {
        options.geojson = Some(read_json("--geojson", file)?);
    }
    if let Some(file) = values.get("--style") {
        options.style = read_json::<Style>("--style", file)?;
    }
    if matches!(options.basemap, Basemap::None) {
        eprintln!(
            "mapshoot: warning: --basemap none disables map tiles; output contains only overlays on a plain background"
        );
    }
    let image = Renderer::default().render(&options)?;
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
