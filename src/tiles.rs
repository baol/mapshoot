use crate::{Options, Result, Viewport, error};
use font8x8::{BASIC_FONTS, UnicodeFonts};
use std::io::Read;
use std::time::Instant;
use tiny_skia::{Color, FilterQuality, Paint, Pixmap, PixmapPaint, Rect, Transform};

struct Tile {
    x: i64,
    y: i64,
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
    let right = ((view.left + f64::from(options.width)) / size).ceil() as i64;
    let top = ((view.top / size).floor() as i64).max(0);
    let bottom = (((view.top + f64::from(options.height)) / size).ceil() as i64).min(count);
    let mut tiles = Vec::new();
    for y in top..bottom {
        for x in left..right {
            tiles.push(Tile { x, y });
        }
    }
    if tiles.len() > 2048 {
        return Err(error(
            "viewport requires more than 2048 tiles; reduce dimensions",
        ));
    }
    let deadline = Instant::now() + options.timeout;
    // Batches bound both concurrent requests and decoded tile memory.
    for batch in tiles.chunks(options.concurrency) {
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|tile| {
                    scope.spawn(move || {
                        let remaining = deadline
                            .checked_duration_since(Instant::now())
                            .ok_or_else(|| error("tile download deadline exceeded"))?;
                        let tile_url = url
                            .replace("{z}", &view.zoom.to_string())
                            .replace("{x}", &tile.x.rem_euclid(count).to_string())
                            .replace("{y}", &tile.y.to_string());
                        let result = fetch(agent, &tile_url, options.tile_size, remaining);
                        // Do not include URLs or HTTP errors: they may contain API keys.
                        result.map_err(|err| {
                            error(format!(
                                "tile {}/{}/{}: {err}",
                                view.zoom,
                                tile.x.rem_euclid(count),
                                tile.y
                            ))
                        })
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().map_err(|_| error("tile worker panicked"))?)
                .collect::<Result<Vec<_>>>()
        })?;
        for (tile, pixels) in batch.iter().zip(results.iter()) {
            let ratio = options.pixel_ratio as f32;
            let x = (tile.x as f64 * size - view.left) as f32 * ratio;
            let y = (tile.y as f64 * size - view.top) as f32 * ratio;
            let paint = PixmapPaint {
                quality: FilterQuality::Nearest,
                ..PixmapPaint::default()
            };
            image.draw_pixmap(
                0,
                0,
                pixels.as_ref(),
                &paint,
                Transform::from_row(ratio, 0.0, 0.0, ratio, x, y),
                None,
            );
        }
    }
    Ok(())
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
        return Err(error("tile exceeds 8 MiB"));
    }
    if data.len() < 24
        || &data[..8] != b"\x89PNG\r\n\x1a\n"
        || &data[12..16] != b"IHDR"
        || u32::from_be_bytes(
            data[16..20]
                .try_into()
                .map_err(|_| error("invalid PNG header"))?,
        ) != size
        || u32::from_be_bytes(
            data[20..24]
                .try_into()
                .map_err(|_| error("invalid PNG header"))?,
        ) != size
    {
        return Err(error(format!("expected a {size}x{size} PNG tile")));
    }
    Pixmap::decode_png(&data).map_err(|_| error("invalid PNG tile data"))
}

fn http_error(err: ureq::Error) -> crate::Error {
    match err {
        ureq::Error::StatusCode(status) => {
            let hint = match status {
                401 | 403 => "check the provider API key and access permissions",
                404 => "check the tile URL, style and supported zoom range",
                429 => "provider rate limit reached; reduce concurrency or retry later",
                500..=599 => "tile provider unavailable; retry later",
                _ => "tile provider rejected the request",
            };
            error(format!("HTTP {status}: {hint}"))
        }
        ureq::Error::Timeout(stage) => error(format!("HTTP timeout ({stage:?})")),
        ureq::Error::Io(err) => error(format!("HTTP I/O failure ({:?})", err.kind())),
        ureq::Error::HostNotFound => error("HTTP hostname could not be resolved"),
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => error("invalid tile URL"),
        ureq::Error::Protocol(_) => error("invalid HTTP response"),
        ureq::Error::Rustls(_) | ureq::Error::Tls(_) | ureq::Error::TlsRequired => {
            error("TLS negotiation failed")
        }
        ureq::Error::InvalidProxyUrl | ureq::Error::ConnectProxyFailed(_) => {
            error("HTTP proxy failed")
        }
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            error("HTTP redirect failed")
        }
        ureq::Error::ConnectionFailed => error("HTTP connection failed"),
        ureq::Error::BodyExceedsLimit(_) | ureq::Error::LargeResponseHeader(_, _) => {
            error("HTTP response exceeds size limits")
        }
        _ => error("HTTP transport failed"),
    }
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
