#![forbid(unsafe_code)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Invalid options, style, attribution or GeoJSON input.
    InvalidInput,
    /// The tile provider answered with a non-success HTTP status.
    Http(u16),
    /// The request failed before a complete response: DNS, connection, TLS,
    /// proxy or redirect problems.
    Network,
    /// The tile download deadline was exceeded.
    Timeout,
    /// Another failure, such as a malformed tile or an internal error.
    Other,
}

#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Machine-readable category; useful to decide whether a retry may help.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// Coordinates are always longitude, latitude.
///
/// `west > east` describes a box that crosses the antimeridian. Longitudes are
/// limited to -180..180 and latitudes to the Web Mercator range
/// (-85.05112878..85.05112878), so the whole world is
/// `-180,-MAX_LATITUDE,180,MAX_LATITUDE`.
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

    /// Spherical box around a point. The box wraps across the antimeridian and
    /// is clipped at the Web Mercator limit instead of being rejected.
    pub fn from_center(longitude: f64, latitude: f64, radius: f64) -> Result<Self> {
        coordinate(longitude, latitude)?;
        if !radius.is_finite() || radius <= 0.0 || radius > 1_000_000.0 {
            return Err(error("radius must be positive and at most 1000000 meters"));
        }
        let angle = radius / 6_371_008.8;
        let dy = angle.to_degrees();
        // Web Mercator cannot show the poles: keep the box inside the
        // renderable latitude range.
        let center = latitude.clamp(-MAX_LATITUDE, MAX_LATITUDE);
        let (west, east) = if latitude.abs() + dy >= 90.0 {
            // A cap containing a pole spans every longitude.
            (-180.0, 180.0)
        } else {
            let dx = (angle.sin() / latitude.to_radians().cos())
                .asin()
                .to_degrees();
            (
                wrap_longitude(longitude - dx),
                wrap_longitude(longitude + dx),
            )
        };
        Self::new(
            west,
            (center - dy).max(-MAX_LATITUDE),
            east,
            (center + dy).min(MAX_LATITUDE),
        )
    }

    /// Box grown by `meters` on every side.
    ///
    /// Longitude degrees per metre shrink towards the poles, so the expansion
    /// uses the edge closest to a pole. Longitudes wrap; latitudes clamp at the
    /// Web Mercator limit. A buffer that covers every longitude yields the
    /// whole world.
    pub fn buffered(self, meters: f64) -> Result<Self> {
        if !meters.is_finite() || !(0.0..=1_000_000.0).contains(&meters) {
            return Err(error("buffer must be 0..1000000 meters"));
        }
        if meters == 0.0 {
            return Ok(self);
        }
        let angle = meters / 6_371_008.8;
        let dy = angle.to_degrees();
        let edge = self.south.abs().max(self.north.abs()).to_radians().cos();
        let dx = (dy / edge).min(180.0);
        let span = if self.west > self.east {
            self.east - self.west + 360.0
        } else {
            self.east - self.west
        };
        let south = (self.south - dy).max(-MAX_LATITUDE);
        let north = (self.north + dy).min(MAX_LATITUDE);
        if span + 2.0 * dx >= 360.0 {
            return Self::new(-180.0, south, 180.0, north);
        }
        Self::new(
            wrap_longitude(self.west - dx),
            south,
            wrap_longitude(self.east + dx),
            north,
        )
    }

    /// Width divided by height of the box in Web Mercator.
    pub fn aspect(&self) -> f64 {
        let (west, north) = project(self.west, self.north, 1.0);
        let (mut east, south) = project(self.east, self.south, 1.0);
        if self.west > self.east {
            east += 1.0;
        }
        (east - west) / (south - north)
    }

    fn validate(self) -> Result<()> {
        for (lon, lat) in [(self.west, self.south), (self.east, self.north)] {
            if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
                return Err(error("bbox longitudes must be finite and within -180..180"));
            }
            if !lat.is_finite() || !(-MAX_LATITUDE..=MAX_LATITUDE).contains(&lat) {
                return Err(error(format!(
                    "bbox latitudes must be within +/-{MAX_LATITUDE} (the Web Mercator limit); \
                     use -180,-{MAX_LATITUDE},180,{MAX_LATITUDE} for the whole world"
                )));
            }
        }
        if self.west == self.east {
            return Err(error(
                "bbox requires west != east; use west > east for a box crossing the antimeridian",
            ));
        }
        if self.west > self.east && self.east - self.west + 360.0 <= 0.0 {
            return Err(error(
                "bbox spans no longitude; use -180,180 for a box covering the whole world",
            ));
        }
        if self.south >= self.north {
            return Err(error("bbox requires south < north"));
        }
        Ok(())
    }
}

/// Fold a longitude into -180..180.
fn wrap_longitude(longitude: f64) -> f64 {
    ((longitude + 180.0).rem_euclid(360.0)) - 180.0
}

pub(crate) fn coordinate(lon: f64, lat: f64) -> Result<()> {
    if !lon.is_finite()
        || !lat.is_finite()
        || !(-180.0..=180.0).contains(&lon)
        || !(-90.0..=90.0).contains(&lat)
    {
        return Err(error(
            "coordinates must be finite: longitude -180..180, latitude -90..90",
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub enum Basemap {
    None,
    Here { api_key: String, style: String },
    Xyz { url: String, attribution: String },
}

impl fmt::Debug for Basemap {
    /// Prints credentials as redacted placeholders.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Here { style, .. } => f
                .debug_struct("Here")
                .field("api_key", &"<redacted>")
                .field("style", style)
                .finish(),
            Self::Xyz { attribution, .. } => f
                .debug_struct("Xyz")
                .field("url", &"<redacted>")
                .field("attribution", attribution)
                .finish(),
        }
    }
}

impl Basemap {
    fn source(&self, tile_size: u32) -> Result<Option<(String, String)>> {
        match self {
            Self::None => Ok(None),
            Self::Here { api_key, style } => {
                if api_key.trim().is_empty() {
                    return Err(error("HERE API key is empty"));
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
    /// Output height. `None` follows from the box aspect ratio, which makes
    /// the image cover exactly the requested box.
    pub height: Option<u32>,
    pub pixel_ratio: u32,
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
            height: None,
            pixel_ratio: 1,
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
        if !(1..=8192).contains(&self.width) || !(1..=4).contains(&self.pixel_ratio) {
            return Err(error("width must be 1..8192 and pixel-ratio 1..4"));
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
    /// Logical output pixels per tile pixel; below one when the image is
    /// rendered at a fractional zoom.
    pub scale: f64,
    /// Position of the image's top-left corner in tile pixels.
    pub left: f64,
    pub top: f64,
    /// Resolved logical output dimensions.
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    pub fn new(options: &Options) -> Result<Self> {
        options.validate()?;
        let b = options.bounds;
        let (west, north) = project(b.west, b.north, 1.0);
        let (mut east, south) = project(b.east, b.south, 1.0);
        if b.west > b.east {
            // A box crossing the antimeridian continues into the next world.
            east += 1.0;
        }
        let box_width = east - west;
        let box_height = south - north;
        let width = options.width;
        let height = match options.height {
            Some(height) => height,
            None => {
                // Follow the box aspect so the image covers it exactly.
                let height = (f64::from(width) * box_height / box_width).round();
                if !(1.0..=8192.0).contains(&height) {
                    return Err(error(format!(
                        "the box needs a {height:.0} pixel high image at width {width}; \
                         pass a smaller --width or an explicit --height / --aspect-ratio"
                    )));
                }
                height as u32
            }
        };
        if !(1..=8192).contains(&height) {
            return Err(error("height must be 1..8192"));
        }
        if u64::from(width) * u64::from(height) * u64::from(options.pixel_ratio).pow(2) > 67_108_864
        {
            return Err(error(
                "image would exceed 64 megapixels; reduce --width, --height or --pixel-ratio",
            ));
        }
        // Fit the box into the frame. When the height follows from the box
        // aspect the frame matches it exactly, up to pixel rounding.
        let scale = (f64::from(width) / box_width).min(f64::from(height) / box_height);
        let tile = f64::from(options.tile_size);
        // Downscale tiles by at most 2x instead of upscaling them; a lower
        // max-zoom falls back to upscaling.
        let zoom = (scale / tile)
            .log2()
            .ceil()
            .clamp(0.0, f64::from(options.max_zoom)) as u8;
        let world_size = tile * 2_f64.powi(i32::from(zoom));
        let tile_scale = scale / world_size;
        let visible_width = f64::from(width) / tile_scale;
        let visible_height = f64::from(height) / tile_scale;
        Ok(Self {
            zoom,
            world_size,
            scale: tile_scale,
            left: (west + east) * world_size / 2.0 - visible_width / 2.0,
            top: (north + south) * world_size / 2.0 - visible_height / 2.0,
            width,
            height,
        })
    }

    /// Width of the image in tile pixels.
    pub(crate) fn visible_width(&self) -> f64 {
        f64::from(self.width) / self.scale
    }

    pub(crate) fn visible_height(&self) -> f64 {
        f64::from(self.height) / self.scale
    }

    pub fn point(&self, longitude: f64, latitude: f64, pixel_ratio: u32) -> (f32, f32) {
        self.point_in_copy(
            longitude,
            latitude,
            self.nearest_copy(longitude),
            pixel_ratio,
        )
    }

    /// Index of the world repetition nearest the view center for a longitude.
    pub(crate) fn nearest_copy(&self, longitude: f64) -> f64 {
        let (x, _) = project(longitude, 0.0, self.world_size);
        let center = self.left + self.visible_width() / 2.0;
        ((center - x) / self.world_size).round()
    }

    /// Pixel position in a specific world repetition; `longitude` may exceed
    /// +/-180 after unwrapping a segment.
    pub(crate) fn point_in_copy(
        &self,
        longitude: f64,
        latitude: f64,
        copy: f64,
        pixel_ratio: u32,
    ) -> (f32, f32) {
        let (x, y) = project(longitude, latitude, self.world_size);
        let x = x + copy * self.world_size;
        (
            ((x - self.left) * self.scale * f64::from(pixel_ratio)) as f32,
            ((y - self.top) * self.scale * f64::from(pixel_ratio)) as f32,
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
            tiles::validate_attribution(attribution, viewport.width, viewport.height)?;
        }
        let mut image = Pixmap::new(
            viewport.width * options.pixel_ratio,
            viewport.height * options.pixel_ratio,
        )
        .ok_or_else(|| Error::new(ErrorKind::Other, "could not allocate output image"))?;
        image.fill(Color::from_rgba8(221, 221, 221, 255));
        if let Some((url, attribution)) = source {
            tiles::compose(&self.agent, &mut image, &viewport, options, &url).map_err(|err| {
                let provider = match options.basemap {
                    Basemap::Here { .. } => "HERE",
                    _ => "XYZ",
                };
                Error::new(err.kind(), format!("{provider} basemap failed: {err}"))
            })?;
            geojson::draw(
                &mut image,
                paths,
                &options.style,
                &viewport,
                options.pixel_ratio,
            )?;
            tiles::attribution(&mut image, &attribution, options.pixel_ratio)?;
        } else {
            geojson::draw(
                &mut image,
                paths,
                &options.style,
                &viewport,
                options.pixel_ratio,
            )?;
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
