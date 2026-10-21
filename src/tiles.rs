use crate::{Options, Result, Viewport, error};
use font8x8::{BASIC_FONTS, UnicodeFonts};
use std::collections::HashMap;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;
use tiny_skia::{Color, FilterQuality, Paint, Pixmap, PixmapPaint, Rect, Transform};

#[derive(Clone, Copy)]
struct Tile {
    /// Wrapped tile column, as used in the request URL and error messages.
    x: i64,
    y: i64,
}

/// One tile request and every position where its pixels are used.
struct Placement {
    tile: Tile,
    /// Device-pixel positions, several when the view is wider than the world.
    origins: Vec<(f32, f32)>,
}

pub(crate) fn compose(
    agent: &ureq::Agent,
    image: &mut Pixmap,
    view: &Viewport,
    options: &Options,
    url: &str,
) -> Result<()> {
    let size = f64::from(options.tile_size);
    let count = 1_i64 << view.zoom;
    let left = (view.left / size).floor() as i64;
    let right = ((view.left + view.visible_width()) / size).ceil() as i64;
    let top = ((view.top / size).floor() as i64).max(0);
    let bottom = (((view.top + view.visible_height()) / size).ceil() as i64).min(count);
    let scale = (view.scale * f64::from(options.pixel_ratio)) as f32;
    let mut placements: Vec<Placement> = Vec::new();
    let mut slots: HashMap<(i64, i64), usize> = HashMap::new();
    let mut placements_count = 0_usize;
    for y in top..bottom {
        for x in left..right {
            placements_count += 1;
            let tile = Tile {
                x: x.rem_euclid(count),
                y,
            };
            let origin = (
                ((x as f64 * size - view.left) * f64::from(scale)) as f32,
                ((y as f64 * size - view.top) * f64::from(scale)) as f32,
            );
            let index = *slots.entry((tile.x, tile.y)).or_insert_with(|| {
                placements.push(Placement {
                    tile,
                    origins: Vec::new(),
                });
                placements.len() - 1
            });
            placements[index].origins.push(origin);
        }
    }
    if placements_count > 2048 {
        return Err(error(
            "viewport requires more than 2048 tiles; reduce dimensions",
        ));
    }
    let deadline = Instant::now() + options.timeout;
    let canceled = AtomicBool::new(false);
    let next = AtomicUsize::new(0);
    let workers = options.concurrency.min(placements.len());
    let mut pending_error = None;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let sender = sender.clone();
            let canceled = &canceled;
            let next = &next;
            let placements = &placements;
            scope.spawn(move || {
                while !canceled.load(Ordering::Acquire) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(placement) = placements.get(index) else {
                        return;
                    };
                    let tile = placement.tile;
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        fetch_tile(agent, url, view.zoom, tile, options.tile_size, deadline)
                    }))
                    .unwrap_or_else(|_| Err(failure("tile worker panicked")));
                    // Do not include URLs or HTTP errors: they may contain API keys.
                    let result = result.map_err(|err| {
                        crate::Error::new(
                            err.kind(),
                            format!("tile {}/{}/{}: {err}", view.zoom, tile.x, tile.y),
                        )
                    });
                    if result.is_err() {
                        canceled.store(true, Ordering::Release);
                    }
                    if sender.send((index, result)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(sender);
        // Draw as tiles arrive: memory stays bounded by the worker count and
        // no slow tile holds back the rest of the view.
        for (index, result) in receiver {
            match result {
                Ok(pixels) if pending_error.is_none() => {
                    draw_tile(image, &placements[index].origins, &pixels, scale);
                }
                Ok(_) => {}
                Err(err) => {
                    pending_error.get_or_insert(err);
                }
            }
        }
    });
    pending_error.map_or(Ok(()), Err)
}

fn draw_tile(image: &mut Pixmap, origins: &[(f32, f32)], pixels: &Pixmap, scale: f32) {
    let paint = PixmapPaint {
        // Whole-tile scales stay crisp; fractional zooms are resampled.
        quality: if (scale - 1.0).abs() < 1e-3 {
            FilterQuality::Nearest
        } else {
            FilterQuality::Bicubic
        },
        ..PixmapPaint::default()
    };
    for &(x, y) in origins {
        image.draw_pixmap(
            0,
            0,
            pixels.as_ref(),
            &paint,
            Transform::from_row(scale, 0.0, 0.0, scale, x, y),
            None,
        );
    }
}

fn fetch_tile(
    agent: &ureq::Agent,
    template: &str,
    zoom: u8,
    tile: Tile,
    size: u32,
    deadline: Instant,
) -> Result<Pixmap> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| {
            crate::Error::new(crate::ErrorKind::Timeout, "tile download deadline exceeded")
        })?;
    let tile_url = template
        .replace("{z}", &zoom.to_string())
        .replace("{x}", &tile.x.to_string())
        .replace("{y}", &tile.y.to_string());
    fetch(agent, &tile_url, size, remaining)
}

fn fetch(
    agent: &ureq::Agent,
    url: &str,
    size: u32,
    timeout: std::time::Duration,
) -> Result<Pixmap> {
    let response = agent
        .get(url)
        .config()
        .timeout_global(Some(timeout))
        .build()
        .call();
    let mut response = match response {
        Ok(response) => response,
        Err(err) => return Err(http_error(err)),
    };
    let mut data = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut data)
        .map_err(|err| http_error(err.into()))?;
    if data.len() > 8 * 1024 * 1024 {
        return Err(failure("tile exceeds 8 MiB"));
    }
    if data.len() < 24
        || &data[..8] != b"\x89PNG\r\n\x1a\n"
        || &data[12..16] != b"IHDR"
        || u32::from_be_bytes(
            data[16..20]
                .try_into()
                .map_err(|_| failure("invalid PNG header"))?,
        ) != size
        || u32::from_be_bytes(
            data[20..24]
                .try_into()
                .map_err(|_| failure("invalid PNG header"))?,
        ) != size
    {
        return Err(failure(format!("expected a {size}x{size} PNG tile")));
    }
    Pixmap::decode_png(&data).map_err(|_| failure("invalid PNG tile data"))
}

fn http_error(err: ureq::Error) -> crate::Error {
    use crate::{Error, ErrorKind};
    match err {
        ureq::Error::StatusCode(status) => {
            let hint = match status {
                401 | 403 => "check the provider API key and access permissions",
                404 => "check the tile URL, style and supported zoom range",
                429 => "provider rate limit reached; reduce concurrency or retry later",
                500..=599 => "tile provider unavailable; retry later",
                _ => "tile provider rejected the request",
            };
            Error::new(ErrorKind::Http(status), format!("HTTP {status}: {hint}"))
        }
        ureq::Error::Timeout(stage) => {
            Error::new(ErrorKind::Timeout, format!("HTTP timeout ({stage:?})"))
        }
        ureq::Error::Io(err) => Error::new(
            ErrorKind::Network,
            format!("HTTP I/O failure ({:?})", err.kind()),
        ),
        ureq::Error::HostNotFound => network("HTTP hostname could not be resolved"),
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => network("invalid tile URL"),
        ureq::Error::Protocol(_) => network("invalid HTTP response"),
        ureq::Error::Rustls(_) | ureq::Error::Tls(_) | ureq::Error::TlsRequired => {
            network("TLS negotiation failed")
        }
        ureq::Error::InvalidProxyUrl | ureq::Error::ConnectProxyFailed(_) => {
            network("HTTP proxy failed")
        }
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            network("HTTP redirect failed")
        }
        ureq::Error::ConnectionFailed => network("HTTP connection failed"),
        ureq::Error::BodyExceedsLimit(_) | ureq::Error::LargeResponseHeader(_, _) => {
            network("HTTP response exceeds size limits")
        }
        _ => network("HTTP transport failed"),
    }
}

fn network(message: impl Into<String>) -> crate::Error {
    crate::Error::new(crate::ErrorKind::Network, message)
}

/// A provider or internal failure, as opposed to invalid user input.
fn failure(message: impl Into<String>) -> crate::Error {
    crate::Error::new(crate::ErrorKind::Other, message)
}

pub(crate) fn validate_attribution(text: &str, width: u32, height: u32) -> Result<()> {
    if !text.bytes().all(|b| (32..=126).contains(&b)) {
        return Err(error(
            "attribution must be printable ASCII; use (c) instead of a copyright symbol",
        ));
    }
    if text.len() as u64 * 8 + 8 > u64::from(width) || height < 16 {
        return Err(error(
            "attribution does not fit; increase image dimensions or shorten attribution",
        ));
    }
    Ok(())
}

pub(crate) fn attribution(image: &mut Pixmap, text: &str, ratio: u32) -> Result<()> {
    let scale = ratio as f32;
    let width = (text.len() * 8 + 8) as f32 * scale;
    let height = 16.0 * scale;
    let left = image.width() as f32 - width;
    let top = image.height() as f32 - height;
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(255, 255, 255, 235));
    image.fill_rect(
        Rect::from_xywh(left, top, width, height)
            .ok_or_else(|| error("invalid attribution rectangle"))?,
        &paint,
        Transform::identity(),
        None,
    );
    paint.set_color(Color::BLACK);
    paint.anti_alias = false;
    for (i, ch) in text.chars().enumerate() {
        let glyph = BASIC_FONTS
            .get(ch)
            .ok_or_else(|| error("unsupported attribution character"))?;
        for (y, bits) in glyph.iter().enumerate() {
            for x in 0..8 {
                if bits & (1 << x) != 0 {
                    let rect = Rect::from_xywh(
                        left + (4 + i * 8 + x) as f32 * scale,
                        top + (4 + y) as f32 * scale,
                        scale,
                        scale,
                    )
                    .ok_or_else(|| error("invalid attribution glyph"))?;
                    image.fill_rect(rect, &paint, Transform::identity(), None);
                }
            }
        }
    }
    Ok(())
}
