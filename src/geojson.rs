use crate::{MAX_LATITUDE, Result, Style, Viewport, coordinate, error, parse_color};
use ab_glyph::{Font, FontArc, PxScale};
use serde_json::Value;
use std::sync::OnceLock;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

const MARKER_SYMBOL_SIZE: f32 = 13.0;
const GRAPHIC_ANTIALIAS: f32 = 1.0;
const MARKER_FONT_DATA: &[u8] = include_bytes!("../assets/SpaceGrotesk-Regular.ttf");
static MARKER_FONT: OnceLock<Option<FontArc>> = OnceLock::new();

pub(crate) enum Shape {
    Point {
        x: f32,
        y: f32,
        style: ShapeStyle,
    },
    Path {
        path: Path,
        fill: bool,
        style: ShapeStyle,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct ShapeStyle {
    fill: Color,
    stroke: Color,
    weight: f32,
    radius: f32,
    symbol: Option<char>,
}

pub(crate) fn prepare(
    value: Option<&Value>,
    viewport: &Viewport,
    ratio: u32,
    style: &Style,
) -> Result<Vec<Shape>> {
    let base = ShapeStyle {
        fill: parse_color(
            style.fill_color.as_deref().unwrap_or(&style.color),
            style.fill_opacity,
        )?,
        stroke: parse_color(&style.color, style.opacity)?,
        weight: style.weight,
        radius: style.radius,
        symbol: None,
    };
    let mut shapes = Vec::new();
    if let Some(value) = value {
        Painter {
            view: viewport,
            ratio,
            out: &mut shapes,
            base,
        }
        .visit(value, 0, false, None)?;
    }
    Ok(shapes)
}

/// Largest logical-pixel extent of the rendered GeoJSON graphics.
///
/// This is the distance from a coordinate position or path centerline to the
/// outer edge of its marker or stroke, plus one pixel for antialiasing.
pub(crate) fn graphic_padding(value: &Value, style: &Style) -> Result<f32> {
    // The viewport only supplies the projected positions; the style extents
    // do not depend on the frame.
    let view = Viewport {
        zoom: 0,
        world_size: 1.0,
        scale: 1.0,
        left: 0.0,
        top: 0.0,
        width: 1,
        height: 1,
    };
    let mut padding = 0.0_f32;
    for shape in prepare(Some(value), &view, 1, style)? {
        let extent = match shape {
            Shape::Point { style, .. } => {
                if style.radius == 0.0 {
                    0.0
                } else {
                    let marker = style.radius + style.weight / 2.0;
                    if style.symbol.is_some() {
                        marker.max(MARKER_SYMBOL_SIZE / 2.0)
                    } else {
                        marker
                    }
                }
            }
            Shape::Path { style, .. } => style.weight / 2.0,
        };
        padding = padding.max(extent);
    }
    Ok(if padding > 0.0 {
        padding + GRAPHIC_ANTIALIAS
    } else {
        0.0
    })
}

struct Painter<'a> {
    view: &'a Viewport,
    ratio: u32,
    out: &'a mut Vec<Shape>,
    base: ShapeStyle,
}

#[derive(Clone, Copy, Default)]
struct FeatureStyle {
    marker_color: Option<[u8; 3]>,
    marker_size: Option<MarkerSize>,
    marker_symbol: Option<char>,
    stroke: Option<[u8; 3]>,
    stroke_opacity: Option<f32>,
    stroke_width: Option<f32>,
    fill: Option<[u8; 3]>,
    fill_opacity: Option<f32>,
}

#[derive(Clone, Copy)]
enum MarkerSize {
    Small,
    Medium,
    Large,
}

impl MarkerSize {
    fn radius(self) -> f32 {
        match self {
            Self::Small => 5.0,
            Self::Medium => 7.0,
            Self::Large => 9.0,
        }
    }
}

impl FeatureStyle {
    fn from_feature(feature: &Value) -> Result<Self> {
        let Some(properties) = feature.get("properties") else {
            return Ok(Self::default());
        };
        if properties.is_null() {
            return Ok(Self::default());
        }
        let properties = properties
            .as_object()
            .ok_or_else(|| error("invalid GeoJSON: properties must be an object or null"))?;
        let mut style = Self::default();
        for (key, value) in properties {
            if value.is_null() {
                continue;
            }
            match key.as_str() {
                "marker-color" => style.marker_color = Some(color_property(key, value)?),
                "marker-size" => {
                    let size = value.as_str().ok_or_else(|| {
                        error("properties.marker-size must be small, medium, or large")
                    })?;
                    style.marker_size = Some(match size {
                        "small" => MarkerSize::Small,
                        "medium" => MarkerSize::Medium,
                        "large" => MarkerSize::Large,
                        _ => {
                            return Err(error(
                                "properties.marker-size must be small, medium, or large",
                            ));
                        }
                    });
                }
                "marker-symbol" => {
                    let symbol = value
                        .as_str()
                        .ok_or_else(|| error("properties.marker-symbol must be a string"))?;
                    let mut chars = symbol.chars();
                    style.marker_symbol = match (chars.next(), chars.next()) {
                        (Some(symbol), None) if symbol.is_ascii_alphanumeric() => Some(symbol),
                        _ => None,
                    };
                }
                "stroke" => style.stroke = Some(color_property(key, value)?),
                "stroke-opacity" => {
                    style.stroke_opacity = Some(number_property(key, value, 0.0, 1.0)?);
                }
                "stroke-width" => {
                    style.stroke_width = Some(number_property(key, value, 0.0, 1024.0)?);
                }
                "fill" => style.fill = Some(color_property(key, value)?),
                "fill-opacity" => {
                    style.fill_opacity = Some(number_property(key, value, 0.0, 1.0)?);
                }
                _ => {}
            }
        }
        Ok(style)
    }

    fn point(self, base: ShapeStyle) -> ShapeStyle {
        let mut style = base;
        if let Some(rgb) = self.marker_color {
            set_rgb(&mut style.fill, rgb);
            set_rgb(&mut style.stroke, rgb);
            // Simplestyle has no marker opacity; markers are solid.
            style.fill.set_alpha(1.0);
            style.stroke.set_alpha(1.0);
        }
        if let Some(size) = self.marker_size {
            style.radius = size.radius();
        }
        if let Some(symbol) = self.marker_symbol {
            style.symbol = Some(symbol);
        }
        style
    }

    fn path(self, base: ShapeStyle) -> ShapeStyle {
        let mut style = base;
        if let Some(rgb) = self.stroke {
            set_rgb(&mut style.stroke, rgb);
        }
        if let Some(opacity) = self.stroke_opacity {
            style.stroke.set_alpha(opacity);
        }
        if let Some(width) = self.stroke_width {
            style.weight = width;
        }
        if let Some(rgb) = self.fill {
            set_rgb(&mut style.fill, rgb);
        }
        if let Some(opacity) = self.fill_opacity {
            style.fill.set_alpha(opacity);
        }
        style
    }
}

fn color_property(key: &str, value: &Value) -> Result<[u8; 3]> {
    let text = value
        .as_str()
        .ok_or_else(|| error(format!("properties.{key} must be a color string")))?;
    simple_color(text).map_err(|err| error(format!("properties.{key}: {err}")))
}

fn number_property(key: &str, value: &Value, min: f32, max: f32) -> Result<f32> {
    let number = value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| error(format!("properties.{key} must be a number")))?;
    if !(f64::from(min)..=f64::from(max)).contains(&number) {
        return Err(error(format!("properties.{key} must be {min}..{max}")));
    }
    Ok(number as f32)
}

/// Simplestyle colors are `#RGB`, `#RRGGBB`, or the same forms without `#`.
fn simple_color(text: &str) -> Result<[u8; 3]> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(error("colors must be #RGB or #RRGGBB"));
    }
    let digit = |index: usize| -> Result<u8> {
        hex.as_bytes()
            .get(index)
            .and_then(|byte| (*byte as char).to_digit(16))
            .map(|value| value as u8)
            .ok_or_else(|| error("colors must be #RGB or #RRGGBB"))
    };
    match hex.len() {
        3 => {
            let r = digit(0)?;
            let g = digit(1)?;
            let b = digit(2)?;
            Ok([r * 17, g * 17, b * 17])
        }
        6 => Ok([
            digit(0)? * 16 + digit(1)?,
            digit(2)? * 16 + digit(3)?,
            digit(4)? * 16 + digit(5)?,
        ]),
        _ => Err(error("colors must be #RGB or #RRGGBB")),
    }
}

fn set_rgb(color: &mut Color, rgb: [u8; 3]) {
    color.set_red(f32::from(rgb[0]) / 255.0);
    color.set_green(f32::from(rgb[1]) / 255.0);
    color.set_blue(f32::from(rgb[2]) / 255.0);
}

/// Geographic extent of every position in a GeoJSON document.
///
/// Longitudes use the shortest arc that contains all positions, matching how
/// lines and polygons are drawn across the antimeridian. Latitude is clamped
/// to the Web Mercator range.
pub(crate) fn extent(value: &Value) -> Result<Option<(f64, f64, f64, f64)>> {
    let mut positions = Vec::new();
    collect_positions(value, &mut positions, 0)?;
    if positions.is_empty() {
        return Ok(None);
    }
    let mut south = f64::INFINITY;
    let mut north = f64::NEG_INFINITY;
    for &(_, latitude) in &positions {
        south = south.min(latitude);
        north = north.max(latitude);
    }
    let (west, east) = shortest_arc(&positions);
    Ok(Some((
        west,
        south.clamp(-MAX_LATITUDE, MAX_LATITUDE),
        east,
        north.clamp(-MAX_LATITUDE, MAX_LATITUDE),
    )))
}

fn collect_positions(value: &Value, out: &mut Vec<(f64, f64)>, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err(error("GeoJSON nesting exceeds 64 levels"));
    }
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| error("invalid GeoJSON: missing type"))?;
    match kind {
        "FeatureCollection" => {
            for feature in array(&value["features"])? {
                if feature["type"] != "Feature" {
                    return Err(error("invalid GeoJSON: features must contain Features"));
                }
                collect_positions(feature, out, depth + 1)?;
            }
        }
        "Feature" => {
            let geometry = value
                .get("geometry")
                .ok_or_else(|| error("invalid GeoJSON: missing geometry"))?;
            if !geometry.is_null() {
                collect_positions(geometry, out, depth + 1)?;
            }
        }
        "GeometryCollection" => {
            for geometry in array(&value["geometries"])? {
                collect_positions(geometry, out, depth + 1)?;
            }
        }
        "Point" => out.push(position(&value["coordinates"])?),
        "MultiPoint" => {
            for point in array(&value["coordinates"])? {
                out.push(position(point)?);
            }
        }
        "MultiLineString" => {
            for line in array(&value["coordinates"])? {
                collect_line(line, out, false)?;
            }
        }
        "MultiPolygon" => {
            for polygon in array(&value["coordinates"])? {
                collect_polygon(polygon, out)?;
            }
        }
        "LineString" => collect_line(&value["coordinates"], out, false)?,
        "Polygon" => collect_polygon(&value["coordinates"], out)?,
        _ => {
            return Err(error(format!(
                "unsupported or invalid GeoJSON type: {kind}"
            )));
        }
    }
    Ok(())
}

fn collect_line(value: &Value, out: &mut Vec<(f64, f64)>, ring: bool) -> Result<()> {
    let points = array(value)?;
    if points.len() < if ring { 4 } else { 2 } {
        return Err(error(
            "invalid GeoJSON: lines need 2 positions, rings need 4",
        ));
    }
    let first = position(&points[0])?;
    if ring {
        let last = position(points.last().ok_or_else(|| error("empty ring"))?)?;
        if first.0 != last.0 || first.1 != last.1 {
            return Err(error("invalid GeoJSON: polygon rings must be closed"));
        }
    }
    out.push(first);
    for point in &points[1..] {
        out.push(position(point)?);
    }
    Ok(())
}

fn collect_polygon(value: &Value, out: &mut Vec<(f64, f64)>) -> Result<()> {
    for ring in array(value)? {
        collect_line(ring, out, true)?;
    }
    Ok(())
}

/// Smallest longitude interval containing all positions.
fn shortest_arc(positions: &[(f64, f64)]) -> (f64, f64) {
    let mut longitudes: Vec<f64> = positions
        .iter()
        .map(|&(longitude, _)| longitude.rem_euclid(360.0))
        .collect();
    longitudes.sort_by(f64::total_cmp);
    let mut gap = f64::NEG_INFINITY;
    let mut start = 0.0;
    for (index, &longitude) in longitudes.iter().enumerate() {
        let next = longitudes
            .get(index + 1)
            .copied()
            .unwrap_or(longitudes[0] + 360.0);
        // Prefer the last of equally wide gaps, which keeps an exact
        // half-world extent on the non-wrapping side.
        if next - longitude >= gap {
            gap = next - longitude;
            start = next;
        }
    }
    let span = 360.0 - gap;
    (signed_longitude(start), signed_longitude(start + span))
}

fn signed_longitude(longitude: f64) -> f64 {
    let wrapped = longitude.rem_euclid(360.0);
    if wrapped > 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}

fn array(value: &Value) -> Result<&Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| error("invalid GeoJSON: expected an array"))
}

fn position(value: &Value) -> Result<(f64, f64)> {
    let values = array(value)?;
    if values.len() < 2 || !values.iter().all(Value::is_number) {
        return Err(error(
            "invalid GeoJSON position: expected at least two numbers",
        ));
    }
    let lon = values[0]
        .as_f64()
        .ok_or_else(|| error("invalid longitude"))?;
    let lat = values[1]
        .as_f64()
        .ok_or_else(|| error("invalid latitude"))?;
    coordinate(lon, lat)?;
    Ok((lon, lat))
}

fn line(
    builder: &mut PathBuilder,
    value: &Value,
    view: &Viewport,
    ratio: u32,
    ring: bool,
) -> Result<()> {
    let points = array(value)?;
    if points.len() < if ring { 4 } else { 2 } {
        return Err(error(
            "invalid GeoJSON: lines need 2 positions, rings need 4",
        ));
    }
    let first = position(&points[0])?;
    if ring {
        let last = position(points.last().ok_or_else(|| error("empty ring"))?)?;
        if first.0 != last.0 || first.1 != last.1 {
            return Err(error("invalid GeoJSON: polygon rings must be closed"));
        }
    }
    // Every vertex uses the same world copy; consecutive longitudes are
    // unwrapped so each segment takes the shortest route around the globe.
    let copy = view.nearest_copy(first.0);
    let (x, y) = view.point_in_copy(first.0, first.1, copy, ratio);
    builder.move_to(x, y);
    let interior = if ring {
        &points[1..points.len() - 1]
    } else {
        &points[1..]
    };
    let mut previous = first.0;
    let mut offset = 0.0;
    for point in interior {
        let (lon, lat) = position(point)?;
        let mut delta = (lon - previous).rem_euclid(360.0);
        if delta > 180.0 {
            delta -= 360.0;
        } else if delta == 180.0 && offset > 0.0 {
            // A half turn is ambiguous; return the way we came so a shape
            // spanning half the world does not wrap around the globe.
            delta = -180.0;
        }
        previous += delta;
        offset += delta;
        let (x, y) = view.point_in_copy(previous, lat, copy, ratio);
        builder.line_to(x, y);
    }
    if ring {
        // The closing edge runs from the last distinct vertex back to the
        // first one, again over the shortest route.
        builder.close();
    }
    Ok(())
}

impl Painter<'_> {
    fn visit(
        &mut self,
        value: &Value,
        depth: usize,
        geometry: bool,
        feature_style: Option<FeatureStyle>,
    ) -> Result<()> {
        if depth > 64 {
            return Err(error("GeoJSON nesting exceeds 64 levels"));
        }
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| error("invalid GeoJSON: missing type"))?;
        match kind {
            "FeatureCollection" if !geometry => {
                for feature in array(&value["features"])? {
                    if feature["type"] != "Feature" {
                        return Err(error("invalid GeoJSON: features must contain Features"));
                    }
                    self.visit(feature, depth + 1, false, None)?;
                }
            }
            "Feature" if !geometry => {
                let style = FeatureStyle::from_feature(value)?;
                let geom = value
                    .get("geometry")
                    .ok_or_else(|| error("invalid GeoJSON: missing geometry"))?;
                if !geom.is_null() {
                    self.visit(geom, depth + 1, true, Some(style))?;
                }
            }
            "GeometryCollection" => {
                for geom in array(&value["geometries"])? {
                    self.visit(geom, depth + 1, true, feature_style)?;
                }
            }
            "Point" => {
                let (lon, lat) = position(&value["coordinates"])?;
                let (x, y) = self.view.point(lon, lat, self.ratio);
                let style = feature_style.map_or(self.base, |style| style.point(self.base));
                self.out.push(Shape::Point { x, y, style });
            }
            "MultiPoint" | "MultiLineString" | "MultiPolygon" => {
                let child = match kind {
                    "MultiPoint" => "Point",
                    "MultiLineString" => "LineString",
                    _ => "Polygon",
                };
                for coordinates in array(&value["coordinates"])? {
                    self.visit(
                        &serde_json::json!({"type": child, "coordinates": coordinates}),
                        depth + 1,
                        true,
                        feature_style,
                    )?;
                }
            }
            "LineString" | "Polygon" => {
                let mut builder = PathBuilder::new();
                let coords = &value["coordinates"];
                if kind == "Polygon" {
                    for ring in array(coords)? {
                        line(&mut builder, ring, self.view, self.ratio, true)?;
                    }
                } else {
                    line(&mut builder, coords, self.view, self.ratio, false)?;
                }
                if let Some(path) = builder.finish() {
                    let style = feature_style.map_or(self.base, |style| style.path(self.base));
                    self.out.push(Shape::Path {
                        path,
                        fill: kind == "Polygon",
                        style,
                    });
                }
            }
            _ => {
                return Err(error(format!(
                    "unsupported or invalid GeoJSON type: {kind}"
                )));
            }
        }
        Ok(())
    }
}

pub(crate) fn draw(
    image: &mut Pixmap,
    shapes: Vec<Shape>,
    view: &Viewport,
    ratio: u32,
) -> Result<()> {
    let mut fill = Paint::default();
    let mut stroke_paint = Paint::default();
    let world = view.world_size * view.scale * f64::from(ratio);
    let width = f64::from(image.width());
    for shape in shapes {
        let (path, should_fill, style, symbol) = match shape {
            Shape::Point { x, y, style } => {
                if style.radius == 0.0 {
                    continue;
                }
                let path = PathBuilder::from_circle(x, y, style.radius * ratio as f32)
                    .ok_or_else(|| error("could not build point circle"))?;
                (path, true, style, Some((x, y)))
            }
            Shape::Path { path, fill, style } => (path, fill, style, None),
        };
        fill.set_color(style.fill);
        stroke_paint.set_color(style.stroke);
        let stroke = Stroke {
            width: style.weight * ratio as f32,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        // Draw every repetition of the world that overlaps the image, so a
        // shape crossing the antimeridian continues on the other side.
        let bounds = path.bounds();
        let first = ((-f64::from(bounds.right())) / world).ceil() as i64;
        let last = ((width - f64::from(bounds.left())) / world).floor() as i64;
        for copy in first..=last {
            let offset = (copy as f64 * world) as f32;
            let transform = Transform::from_translate(offset, 0.0);
            if should_fill {
                image.fill_path(&path, &fill, FillRule::EvenOdd, transform, None);
            }
            if style.weight > 0.0 {
                image.stroke_path(&path, &stroke_paint, &stroke, transform, None);
            }
            if let Some((x, y)) = symbol {
                marker_symbol(image, style.symbol, x + offset, y, ratio, style.fill)?;
            }
        }
    }
    Ok(())
}

fn marker_symbol(
    image: &mut Pixmap,
    symbol: Option<char>,
    x: f32,
    y: f32,
    ratio: u32,
    fill: Color,
) -> Result<()> {
    let Some(symbol) = symbol else {
        return Ok(());
    };
    let Some(font) = MARKER_FONT
        .get_or_init(|| FontArc::try_from_slice(MARKER_FONT_DATA).ok())
        .as_ref()
    else {
        return Err(error("invalid embedded marker font"));
    };
    let glyph = font
        .glyph_id(symbol)
        .with_scale(PxScale::from(MARKER_SYMBOL_SIZE * ratio as f32));
    let Some(outline) = font.outline_glyph(glyph) else {
        return Ok(());
    };
    let bounds = outline.px_bounds();
    if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return Ok(());
    }
    // Center the visible outline, not the font's layout box: digits and
    // letters then share the same optical center and cap height.
    let left = (x - bounds.width() / 2.0).round();
    let top = (y - bounds.height() / 2.0).round();
    let luminance = 0.2126 * fill.red() + 0.7152 * fill.green() + 0.0722 * fill.blue();
    let text = if luminance < 0.5 {
        Color::WHITE
    } else {
        Color::BLACK
    };
    let mut paint = Paint {
        anti_alias: false,
        ..Paint::default()
    };
    outline.draw(|column, row, coverage| {
        let coverage = coverage.clamp(0.0, 1.0);
        if coverage <= 0.0 {
            return;
        }
        let mut color = text;
        color.set_alpha(coverage);
        paint.set_color(color);
        if let Some(rect) = Rect::from_xywh(left + column as f32, top + row as f32, 1.0, 1.0) {
            image.fill_rect(rect, &paint, Transform::identity(), None);
        }
    });
    Ok(())
}
