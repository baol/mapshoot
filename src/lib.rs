//! Native Web Mercator raster maps. No browser, GUI, or async runtime.
mod geojson;
mod tiles;

use serde::Deserialize;
use std::fmt;
use std::time::Duration;
use tiny_skia::{Color, Pixmap};

pub const MAX_LATITUDE: f64 = 85.0511287798066;
pub const HERE_STYLES: &[&str] = &[
    "explore.day",
    "explore.night",
    "explore.satellite.day",
    "lite.day",
    "lite.night",
    "lite.satellite.day",
    "logistics.day",
    "logistics.night",
    "logistics.satellite.day",
    "satellite.day",
    "topo.day",
    "topo.night",
    "dem",
];

#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn error(message: impl Into<String>) -> Error {
    Error(message.into())
}

/// Coordinates are always longitude, latitude. Antimeridian crossing is rejected.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

impl Bounds {
    pub fn new(west: f64, south: f64, east: f64, north: f64) -> Result<Self> {
        let bounds = Self {
            west,
            south,
            east,
            north,
        };
        bounds.validate()?;
        Ok(bounds)
    }

    pub fn from_center(longitude: f64, latitude: f64, radius: f64) -> Result<Self> {
        coordinate(longitude, latitude)?;
        if !radius.is_finite() || radius <= 0.0 || radius > 1_000_000.0 {
            return Err(error("radius must be positive and at most 1000000 meters"));
        }
        let angle = radius / 6_371_008.8;
        let lat = latitude.to_radians();
        if lat.abs() + angle >= std::f64::consts::FRAC_PI_2 {
            return Err(error("radius crosses a pole"));
        }
        let dy = angle.to_degrees();
        let dx = (angle.sin() / lat.cos()).asin().to_degrees();
        Self::new(longitude - dx, latitude - dy, longitude + dx, latitude + dy)
    }

    fn validate(self) -> Result<()> {
        coordinate(self.west, self.south)?;
        coordinate(self.east, self.north)?;
        if self.west >= self.east || self.south >= self.north {
            return Err(error(
                "bbox requires west < east and south < north; antimeridian crossing is unsupported",
            ));
        }
        Ok(())
    }
}

pub(crate) fn coordinate(lon: f64, lat: f64) -> Result<()> {
    if !lon.is_finite()
        || !lat.is_finite()
        || !(-180.0..=180.0).contains(&lon)
        || !(-MAX_LATITUDE..=MAX_LATITUDE).contains(&lat)
    {
        return Err(error(
            "coordinates must be finite: longitude -180..180, latitude -85.05112878..85.05112878",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub enum Basemap {
    None,
    Here { api_key: String, style: String },
    Xyz { url: String, attribution: String },
}

impl Basemap {
    fn source(&self, tile_size: u32) -> Result<Option<(String, String)>> {
        match self {
            Self::None => Ok(None),
            Self::Here { api_key, style } => {
                if api_key.trim().is_empty() {
                    return Err(error(
                        "HERE API key is empty; provide a valid api_key or set HERE_API_KEY for the CLI",
                    ));
                }
                if !HERE_STYLES.contains(&style.as_str()) {
                    return Err(error(format!("unknown HERE style: {style}")));
                }
                let key: String = api_key
                    .bytes()
                    .map(|b| {
                        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                            char::from(b).to_string()
                        } else {
                            format!("%{b:02X}")
                        }
                    })
                    .collect();
                Ok(Some((
                    format!(
                        "https://maps.hereapi.com/v3/base/mc/{{z}}/{{x}}/{{y}}/png?apiKey={key}&style={style}&size={tile_size}"
                    ),
                    "(c) HERE".into(),
                )))
            }
            Self::Xyz { url, attribution } => {
                if !(url.starts_with("https://") || url.starts_with("http://"))
                    || !["{z}", "{x}", "{y}"].iter().all(|s| url.contains(s))
                {
                    return Err(error("XYZ URL must use HTTP(S) and contain {z}, {x}, {y}"));
                }
                if attribution.trim().is_empty() {
                    return Err(error("XYZ attribution is required (plain ASCII text)"));
                }
                Ok(Some((url.clone(), attribution.clone())))
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Style {
    pub color: String,
    pub fill_color: Option<String>,
    pub weight: f32,
    pub opacity: f32,
    pub fill_opacity: f32,
    pub radius: f32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: "#3388ff".into(),
            fill_color: None,
            weight: 3.0,
            opacity: 1.0,
            fill_opacity: 0.2,
            radius: 6.0,
        }
    }
}

impl Style {
    fn validate(&self) -> Result<()> {
        parse_color(&self.color, self.opacity)?;
        parse_color(
            self.fill_color.as_deref().unwrap_or(&self.color),
            self.fill_opacity,
        )?;
        if !self.weight.is_finite()
            || !(0.0..=1024.0).contains(&self.weight)
            || !self.radius.is_finite()
            || !(0.0..=4096.0).contains(&self.radius)
        {
            return Err(error("style weight must be 0..1024 and radius 0..4096"));
        }
        Ok(())
    }
}

pub(crate) fn parse_color(text: &str, opacity: f32) -> Result<Color> {
    if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
        return Err(error("style opacity and fillOpacity must be 0..1"));
    }
    let hex = text
        .strip_prefix('#')
        .ok_or_else(|| error("colors must be #RRGGBB"))?;
    if hex.len() != 6 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(error("colors must be #RRGGBB"));
    }
    let rgb = u32::from_str_radix(hex, 16).map_err(|_| error("invalid hex color"))?;
    Ok(Color::from_rgba8(
        (rgb >> 16) as u8,
        (rgb >> 8) as u8,
        rgb as u8,
        (opacity * 255.0).round() as u8,
    ))
}

#[derive(Clone, Debug)]
pub struct Options {
    pub bounds: Bounds,
    pub basemap: Basemap,
    pub width: u32,
    pub height: u32,
    pub pixel_ratio: u32,
    pub padding: u32,
    pub max_zoom: u8,
    pub tile_size: u32,
    pub concurrency: usize,
    /// Deadline for the complete tile download phase, not per tile.
    pub timeout: Duration,
    pub geojson: Option<serde_json::Value>,
    pub style: Style,
}

impl Options {
    pub fn new(bounds: Bounds, basemap: Basemap) -> Self {
        Self {
            bounds,
            basemap,
            width: 1024,
            height: 768,
            pixel_ratio: 1,
            padding: 20,
            max_zoom: 20,
            tile_size: 256,
            concurrency: 8,
            timeout: Duration::from_secs(15),
            geojson: None,
            style: Style::default(),
        }
    }

    fn validate(&self) -> Result<()> {
        self.bounds.validate()?;
        if !(1..=8192).contains(&self.width)
            || !(1..=8192).contains(&self.height)
            || !(1..=4).contains(&self.pixel_ratio)
            || u64::from(self.width) * u64::from(self.height) * u64::from(self.pixel_ratio).pow(2)
                > 67_108_864
        {
            return Err(error(
                "dimensions must be 1..8192, pixel-ratio 1..4, total at most 64 megapixels",
            ));
        }
        if self.padding.saturating_mul(2) >= self.width.min(self.height) {
            return Err(error("padding leaves no map area"));
        }
        if self.max_zoom > 22 || ![256, 512].contains(&self.tile_size) {
            return Err(error("max-zoom must be 0..22 and tile-size 256 or 512"));
        }
        if !(1..=32).contains(&self.concurrency) {
            return Err(error("concurrency must be 1..32"));
        }
        if self.timeout.is_zero() || self.timeout > Duration::from_secs(300) {
            return Err(error("timeout must be 1..300000 milliseconds"));
        }
        self.style.validate()
    }
}

pub(crate) fn project(lon: f64, lat: f64, world: f64) -> (f64, f64) {
    let x = (lon + 180.0) / 360.0 * world;
    let y = (1.0 - lat.to_radians().tan().asinh() / std::f64::consts::PI) / 2.0 * world;
    (x, y.clamp(0.0, world))
}

#[derive(Debug)]
pub struct Viewport {
    pub zoom: u8,
    pub world_size: f64,
    pub left: f64,
    pub top: f64,
}

impl Viewport {
    pub fn new(options: &Options) -> Result<Self> {
        options.validate()?;
        let b = options.bounds;
        let (west, north) = project(b.west, b.north, 1.0);
        let (east, south) = project(b.east, b.south, 1.0);
        let available_x = f64::from(options.width - options.padding * 2);
        let available_y = f64::from(options.height - options.padding * 2);
        let scale = (available_x / (east - west)).min(available_y / (south - north));
        let zoom = (scale / f64::from(options.tile_size))
            .log2()
            .floor()
            .clamp(0.0, f64::from(options.max_zoom)) as u8;
        let world_size = f64::from(options.tile_size) * 2_f64.powi(i32::from(zoom));
        if (east - west) * world_size > available_x + 1e-6
            || (south - north) * world_size > available_y + 1e-6
        {
            return Err(error(
                "area cannot fit at zoom 0; increase dimensions or reduce padding",
            ));
        }
        Ok(Self {
            zoom,
            world_size,
            left: ((west + east) * world_size / 2.0 - f64::from(options.width) / 2.0).floor(),
            top: ((north + south) * world_size / 2.0 - f64::from(options.height) / 2.0).floor(),
        })
    }

    pub fn point(&self, longitude: f64, latitude: f64, pixel_ratio: u32) -> (f32, f32) {
        let (x, y) = project(longitude, latitude, self.world_size);
        (
            ((x - self.left) * f64::from(pixel_ratio)) as f32,
            ((y - self.top) * f64::from(pixel_ratio)) as f32,
        )
    }
}

/// Reuses HTTP connections across renders. All drawing is CPU-based.
pub struct Renderer {
    agent: ureq::Agent,
}

impl Default for Renderer {
    fn default() -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .user_agent(concat!("mapshoot/", env!("CARGO_PKG_VERSION")))
                .build()
                .new_agent(),
        }
    }
}

impl Renderer {
    /// Use a custom HTTP agent, for example to set a provider-specific User-Agent.
    pub fn with_agent(agent: ureq::Agent) -> Self {
        Self { agent }
    }

    /// Render an image in memory. No output is produced if any tile or overlay fails.
    pub fn render(&self, options: &Options) -> Result<Pixmap> {
        let viewport = Viewport::new(options)?;
        let source = options.basemap.source(options.tile_size)?;
        let paths = geojson::prepare(options.geojson.as_ref(), &viewport, options.pixel_ratio)?;
        if let Some((_, attribution)) = &source {
            tiles::validate_attribution(attribution, options.width, options.height)?;
        }
        let mut image = Pixmap::new(
            options.width * options.pixel_ratio,
            options.height * options.pixel_ratio,
        )
        .ok_or_else(|| error("could not allocate output image"))?;
        image.fill(Color::from_rgba8(221, 221, 221, 255));
        if let Some((url, attribution)) = source {
            tiles::compose(&self.agent, &mut image, &viewport, options, &url).map_err(|err| {
                let provider = match options.basemap {
                    Basemap::Here { .. } => "HERE",
                    _ => "XYZ",
                };
                error(format!("{provider} basemap failed: {err}"))
            })?;
            geojson::draw(&mut image, paths, &options.style, options.pixel_ratio)?;
            tiles::attribution(&mut image, &attribution, options.pixel_ratio)?;
        } else {
            geojson::draw(&mut image, paths, &options.style, options.pixel_ratio)?;
        }
        Ok(image)
    }
}

pub fn render(options: &Options) -> Result<Pixmap> {
    Renderer::default().render(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn here_templates_encode_credentials_and_include_each_style() {
        for style in HERE_STYLES {
            let here = Basemap::Here {
                api_key: "a&b /?é".into(),
                style: (*style).into(),
            };
            for tile_size in [256, 512] {
                let (url, attribution) = here.source(tile_size).unwrap().unwrap();
                assert!(url.starts_with("https://maps.hereapi.com/v3/base/mc/{z}/{x}/{y}/png?"));
                assert!(url.contains("apiKey=a%26b%20%2F%3F%C3%A9&style="));
                assert!(
                    url.ends_with(&format!("style={style}&size={tile_size}")),
                    "{url}"
                );
                assert_eq!(attribution, "(c) HERE");
            }
        }
        assert!(Basemap::None.source(256).unwrap().is_none());
    }
}
