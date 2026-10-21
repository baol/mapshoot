use crate::{Result, Style, Viewport, coordinate, error, parse_color};
use serde_json::Value;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

pub(crate) enum Shape {
    Point(f32, f32),
    Path { path: Path, fill: bool },
}

pub(crate) fn prepare(
    value: Option<&Value>,
    viewport: &Viewport,
    ratio: u32,
) -> Result<Vec<Shape>> {
    let mut shapes = Vec::new();
    if let Some(value) = value {
        visit(value, viewport, ratio, &mut shapes, 0, false)?;
    }
    Ok(shapes)
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

fn visit(
    value: &Value,
    view: &Viewport,
    ratio: u32,
    out: &mut Vec<Shape>,
    depth: usize,
    geometry: bool,
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
                visit(feature, view, ratio, out, depth + 1, false)?;
            }
        }
        "Feature" if !geometry => {
            let geom = value
                .get("geometry")
                .ok_or_else(|| error("invalid GeoJSON: missing geometry"))?;
            if !geom.is_null() {
                visit(geom, view, ratio, out, depth + 1, true)?;
            }
        }
        "GeometryCollection" => {
            for geom in array(&value["geometries"])? {
                visit(geom, view, ratio, out, depth + 1, true)?;
            }
        }
        "Point" => {
            let (lon, lat) = position(&value["coordinates"])?;
            let (x, y) = view.point(lon, lat, ratio);
            out.push(Shape::Point(x, y));
        }
        "MultiPoint" | "MultiLineString" | "MultiPolygon" => {
            let child = match kind {
                "MultiPoint" => "Point",
                "MultiLineString" => "LineString",
                _ => "Polygon",
            };
            for coordinates in array(&value["coordinates"])? {
                visit(
                    &serde_json::json!({"type": child, "coordinates": coordinates}),
                    view,
                    ratio,
                    out,
                    depth + 1,
                    true,
                )?;
            }
        }
        "LineString" | "Polygon" => {
            let mut builder = PathBuilder::new();
            let coords = &value["coordinates"];
            if kind == "Polygon" {
                for ring in array(coords)? {
                    line(&mut builder, ring, view, ratio, true)?;
                }
            } else {
                line(&mut builder, coords, view, ratio, false)?;
            }
            if let Some(path) = builder.finish() {
                out.push(Shape::Path {
                    path,
                    fill: kind == "Polygon",
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

pub(crate) fn draw(
    image: &mut Pixmap,
    shapes: Vec<Shape>,
    style: &Style,
    view: &Viewport,
    ratio: u32,
) -> Result<()> {
    let mut fill = Paint::default();
    fill.set_color(parse_color(
        style.fill_color.as_deref().unwrap_or(&style.color),
        style.fill_opacity,
    )?);
    let mut stroke_paint = Paint::default();
    stroke_paint.set_color(parse_color(&style.color, style.opacity)?);
    let stroke = Stroke {
        width: style.weight * ratio as f32,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let world = view.world_size * view.scale * f64::from(ratio);
    let width = f64::from(image.width());
    for shape in shapes {
        let (path, should_fill) = match shape {
            Shape::Point(x, y) => {
                if style.radius == 0.0 {
                    continue;
                }
                (
                    PathBuilder::from_circle(x, y, style.radius * ratio as f32)
                        .ok_or_else(|| error("could not build point circle"))?,
                    true,
                )
            }
            Shape::Path { path, fill } => (path, fill),
        };
        // Draw every repetition of the world that overlaps the image, so a
        // shape crossing the antimeridian continues on the other side.
        let bounds = path.bounds();
        let first = ((-f64::from(bounds.right())) / world).ceil() as i64;
        let last = ((width - f64::from(bounds.left())) / world).floor() as i64;
        for copy in first..=last {
            let transform = Transform::from_translate((copy as f64 * world) as f32, 0.0);
            if should_fill {
                image.fill_path(&path, &fill, FillRule::EvenOdd, transform, None);
            }
            if style.weight > 0.0 {
                image.stroke_path(&path, &stroke_paint, &stroke, transform, None);
            }
        }
    }
    Ok(())
}
