use ab_glyph::{Font, FontArc, Glyph, PxScale, ScaleFont, point};
use anyhow::Result;
use base64::{Engine as _, engine::general_purpose};
use image::{Rgba, RgbaImage};
use std::collections::HashMap;
use std::fmt;
use std::io::Cursor;
use std::sync::{LazyLock, Mutex, OnceLock};

/// Lazily-loaded sans-serif bold font. Tries a list of common Linux system paths;
/// returns None if none are readable, in which case text rendering is a no-op.
static TITLE_FONT: LazyLock<Option<FontArc>> = LazyLock::new(|| {
    let paths: &[&str] = &[
        "/usr/share/fonts/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
    ];
    for path in paths {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(font) = FontArc::try_from_vec(bytes) {
                return Some(font);
            }
        }
    }
    None
});

pub fn measure_text_width(font: &FontArc, text: &str, scale: PxScale) -> f32 {
    let scaled = font.as_scaled(scale);
    let mut width = 0.0f32;
    let mut prev: Option<ab_glyph::GlyphId> = None;
    for c in text.chars() {
        let gid = font.glyph_id(c);
        if let Some(p) = prev {
            width += scaled.kern(p, gid);
        }
        width += scaled.h_advance(gid);
        prev = Some(gid);
    }
    width
}

fn fit_text(font: &FontArc, text: &str, scale: PxScale, max_w: f32) -> String {
    if measure_text_width(font, text, scale) <= max_w {
        return text.to_string();
    }
    let mut s = text.to_string();
    while measure_text_width(font, &s, scale) > max_w && !s.is_empty() {
        s.pop();
    }
    s
}

/// Return a reference to the loaded title font, if available.
pub fn title_font() -> Option<&'static FontArc> {
    TITLE_FONT.as_ref()
}

/// Draw text horizontally centered within the rectangle
/// (area_x, area_y, area_w, area_h), alpha-blended onto `img`.
/// No-op if the system font is unavailable or the text is empty.
fn draw_text_centered(img: &mut RgbaImage, text: &str, area_x: u32, area_y: u32, area_w: u32, size_px: f32, color: Rgba<u8>) {
    let Some(font) = TITLE_FONT.as_ref() else {
        return;
    };
    if text.is_empty() {
        return;
    }

    let scale = PxScale::from(size_px);
    let fitted = fit_text(font, text, scale, area_w as f32 - 4.0);
    if fitted.is_empty() {
        return;
    }

    let scaled = font.as_scaled(scale);
    let ascent = scaled.ascent();
    let width = measure_text_width(font, &fitted, scale);
    let x_start = area_x as f32 + (area_w as f32 - width) / 2.0;
    let y_baseline = area_y as f32 + ascent + 1.0;

    let mut x_cursor = x_start;
    let mut prev: Option<ab_glyph::GlyphId> = None;
    for c in fitted.chars() {
        let gid = font.glyph_id(c);
        if let Some(p) = prev {
            x_cursor += scaled.kern(p, gid);
        }
        let glyph: Glyph = gid.with_scale_and_position(scale, point(x_cursor, y_baseline));

        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, coverage| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                    let bg = *img.get_pixel(px as u32, py as u32);
                    let a = coverage * (color[3] as f32 / 255.0);
                    let r = (color[0] as f32 * a + bg[0] as f32 * (1.0 - a)) as u8;
                    let g = (color[1] as f32 * a + bg[1] as f32 * (1.0 - a)) as u8;
                    let b = (color[2] as f32 * a + bg[2] as f32 * (1.0 - a)) as u8;
                    img.put_pixel(px as u32, py as u32, Rgba([r, g, b, bg[3]]));
                }
            });
        }
        x_cursor += scaled.h_advance(gid);
        prev = Some(gid);
    }
}

/// Draw text scrolling horizontally within a clipped region, with seamless wrap-around.
/// `scroll_offset` is in pixels; the text repeats after `text_width + gap`.
fn draw_text_scrolling(
    img: &mut RgbaImage,
    text: &str,
    area_x: u32,
    area_y: u32,
    area_w: u32,
    size_px: f32,
    color: Rgba<u8>,
    scroll_offset: f32,
    text_width: f32,
    gap: f32,
) {
    let Some(font) = TITLE_FONT.as_ref() else {
        return;
    };
    if text.is_empty() {
        return;
    }

    let scale = PxScale::from(size_px);
    let scaled = font.as_scaled(scale);
    let ascent = scaled.ascent();
    let y_baseline = area_y as f32 + ascent + 1.0;
    let cycle = text_width + gap;

    // Draw two copies of the text for seamless scrolling
    for copy in 0..2 {
        let x_start = area_x as f32 + 2.0 - scroll_offset + copy as f32 * cycle;

        // Skip if this copy is entirely off-screen
        if x_start > area_x as f32 + area_w as f32 {
            continue;
        }
        if x_start + text_width < area_x as f32 {
            continue;
        }

        let mut x_cursor = x_start;
        let mut prev: Option<ab_glyph::GlyphId> = None;
        for c in text.chars() {
            let gid = font.glyph_id(c);
            if let Some(p) = prev {
                x_cursor += scaled.kern(p, gid);
            }
            let glyph: Glyph = gid.with_scale_and_position(scale, point(x_cursor, y_baseline));

            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    let px = bounds.min.x as i32 + gx as i32;
                    let py = bounds.min.y as i32 + gy as i32;
                    // Clip to area_x..area_x+area_w horizontally
                    if px >= area_x as i32
                        && (px as u32) < area_x + area_w
                        && py >= 0
                        && (py as u32) < img.height()
                    {
                        let bg = *img.get_pixel(px as u32, py as u32);
                        let a = coverage * (color[3] as f32 / 255.0);
                        let r = (color[0] as f32 * a + bg[0] as f32 * (1.0 - a)) as u8;
                        let g = (color[1] as f32 * a + bg[1] as f32 * (1.0 - a)) as u8;
                        let b = (color[2] as f32 * a + bg[2] as f32 * (1.0 - a)) as u8;
                        img.put_pixel(px as u32, py as u32, Rgba([r, g, b, bg[3]]));
                    }
                });
            }
            x_cursor += scaled.h_advance(gid);
            prev = Some(gid);
        }
    }
}

static VOLUME_BAR_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

pub static TRANSPARENT_ICON: LazyLock<String> = LazyLock::new(|| {
    const ICON_SIZE: u32 = 144;
    let img = RgbaImage::from_pixel(ICON_SIZE, ICON_SIZE, Rgba([0, 0, 0, 0]));

    let mut buffer = Vec::new();
    let mut cursor = Cursor::new(&mut buffer);
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("Failed to encode transparent icon");

    let base64 = general_purpose::STANDARD.encode(&buffer);
    format!("data:image/png;base64,{}", base64)
});

enum BarPosition {
    Upper,
    Lower,
}

impl fmt::Display for BarPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BarPosition::Upper => write!(f, "Upper"),
            BarPosition::Lower => write!(f, "Lower"),
        }
    }
}

/// Get data URI format for split volume bar images
pub fn get_volume_bar_data_uri_split(volume_percent: f32) -> Result<(String, String)> {
    let upper_key = generate_cache_key(volume_percent, BarPosition::Upper);
    let lower_key = generate_cache_key(volume_percent, BarPosition::Lower);

    if let (Ok(Some(cached_upper)), Ok(Some(cached_lower))) = (
        get_cached_value_safe(&upper_key),
        get_cached_value_safe(&lower_key),
    ) {
        return Ok((cached_upper, cached_lower));
    }

    let (top_base64, bottom_base64) = get_volume_bar_base64_split(volume_percent)?;
    let top_data_uri = format!("data:image/png;base64,{}", top_base64);
    let bottom_data_uri = format!("data:image/png;base64,{}", bottom_base64);

    set_cached_value(upper_key, top_data_uri.clone())
        .expect("Failed to retrieve cached upper part of volume bar");
    set_cached_value(lower_key, bottom_data_uri.clone())
        .expect("Failed to retrieve cached lower part of volume bar");

    Ok((top_data_uri, bottom_data_uri))
}

fn set_cached_value(key: String, value: String) -> Result<(), String> {
    match get_cache().lock() {
        Ok(mut cache) => {
            cache.insert(key, value);
            Ok(())
        }
        Err(_) => Err("Failed to acquire cache lock".to_string()),
    }
}

fn get_cache() -> &'static Mutex<HashMap<String, String>> {
    VOLUME_BAR_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn generate_cache_key(volume_percent: f32, position: BarPosition) -> String {
    format!("vol_{:.1}_part_{}", volume_percent, position)
}

fn get_cached_value_safe(key: &str) -> Result<Option<String>, String> {
    match get_cache().lock() {
        Ok(cache) => Ok(cache.get(key).cloned()),
        Err(_) => Err("Failed to acquire cache lock".to_string()),
    }
}

/// Blend two colors with alpha blending
fn blend_colors(bg: Rgba<u8>, fg: Rgba<u8>, alpha: f32) -> Rgba<u8> {
    let alpha = alpha.clamp(0.0, 1.0);

    // If background is fully transparent, just return foreground with adjusted alpha
    if bg[3] == 0 {
        return Rgba([fg[0], fg[1], fg[2], (fg[3] as f32 * alpha) as u8]);
    }

    let fg_alpha = (fg[3] as f32 / 255.0) * alpha;
    let bg_alpha = bg[3] as f32 / 255.0;
    let final_alpha = fg_alpha + bg_alpha * (1.0 - fg_alpha);

    if final_alpha == 0.0 {
        return Rgba([0, 0, 0, 0]);
    }

    let r = ((fg[0] as f32 * fg_alpha + bg[0] as f32 * bg_alpha * (1.0 - fg_alpha)) / final_alpha)
        as u8;
    let g = ((fg[1] as f32 * fg_alpha + bg[1] as f32 * bg_alpha * (1.0 - fg_alpha)) / final_alpha)
        as u8;
    let b = ((fg[2] as f32 * fg_alpha + bg[2] as f32 * bg_alpha * (1.0 - fg_alpha)) / final_alpha)
        as u8;
    let a = (final_alpha * 255.0) as u8;

    Rgba([r, g, b, a])
}

/// Calculate signed distance from a point to a rounded rectangle
/// Negative values mean inside, positive values mean outside
fn volume_bar_distance(
    px: f32,
    py: f32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
) -> f32 {
    let dx = (px - x - width / 2.0).abs() - (width / 2.0 - radius);
    let dy = (py - y - height / 2.0).abs() - (height / 2.0 - radius);

    let outside_dist = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
    let inside_dist = dx.max(dy).min(0.0);

    outside_dist + inside_dist - radius
}

/// Generate a volume bar image spanning 2 Stream Deck icons (288x144 total)
/// Returns (top_image, bottom_image) as separate 144x144 images
pub fn generate_volume_bar_split(volume_percent: f32) -> (RgbaImage, RgbaImage) {
    const ICON_WIDTH: u32 = 144;
    const ICON_HEIGHT: u32 = 144;
    const TOTAL_HEIGHT: u32 = 288;
    const BAR_WIDTH: u32 = 26;
    const BAR_HEIGHT: u32 = 240;
    const POINTER_RADIUS: u32 = 20;
    const OUTLINE_THICKNESS: u32 = 6;

    let mut full_img = RgbaImage::from_pixel(ICON_WIDTH, TOTAL_HEIGHT, Rgba([0, 0, 0, 0]));

    let bar_x = (ICON_WIDTH - BAR_WIDTH) / 2;
    let bar_y = (TOTAL_HEIGHT - BAR_HEIGHT) / 2;

    let bar_fill = Rgba([255, 255, 255, 255]);
    let bar_outline = Rgba([255, 255, 255, 255]);
    let circle_outline = Rgba([255, 255, 255, 255]);

    draw_volume_bar_outline(
        &mut full_img,
        bar_x,
        bar_y,
        BAR_WIDTH,
        BAR_HEIGHT,
        BAR_WIDTH / 2,
        bar_outline,
        OUTLINE_THICKNESS,
    );

    // Calculate and draw the filled portion
    let fill_height = ((volume_percent / 100.0) * BAR_HEIGHT as f32) as u32;
    let fill_y = bar_y + BAR_HEIGHT - fill_height;

    if fill_height > OUTLINE_THICKNESS {
        for py in fill_y.max(bar_y + OUTLINE_THICKNESS)..(bar_y + BAR_HEIGHT - OUTLINE_THICKNESS) {
            for px in (bar_x + OUTLINE_THICKNESS)..(bar_x + BAR_WIDTH - OUTLINE_THICKNESS + 1) {
                if px < full_img.width() && py < full_img.height() {
                    full_img.put_pixel(px, py, bar_fill);
                }
            }
        }
    }

    // Draw the volume indicator circle
    let circle_x = bar_x + BAR_WIDTH / 2;
    let circle_y = fill_y;

    draw_volume_pointer(
        &mut full_img,
        circle_x,
        circle_y,
        POINTER_RADIUS,
        Rgba([0, 0, 0, 255]),
        circle_outline,
        OUTLINE_THICKNESS,
    );

    // Split into top and bottom images
    let mut top_img = RgbaImage::from_pixel(ICON_WIDTH, ICON_HEIGHT, Rgba([0, 0, 0, 0]));
    let mut bottom_img = RgbaImage::from_pixel(ICON_WIDTH, ICON_HEIGHT, Rgba([0, 0, 0, 0]));

    for y in 0..ICON_HEIGHT {
        for x in 0..ICON_WIDTH {
            top_img.put_pixel(x, y, *full_img.get_pixel(x, y));
            bottom_img.put_pixel(x, y, *full_img.get_pixel(x, y + ICON_HEIGHT));
        }
    }

    (top_img, bottom_img)
}

/// Draw a filled circle with outline and antialiasing
fn draw_volume_pointer(
    img: &mut RgbaImage,
    center_x: u32,
    center_y: u32,
    radius: u32,
    fill_color: Rgba<u8>,
    outline_color: Rgba<u8>,
    outline_thickness: u32,
) {
    let cx = center_x as f32;
    let cy = center_y as f32;
    let outer_r = radius as f32;
    let inner_r = (radius as f32) - (outline_thickness as f32);

    let min_x = (cx - outer_r - 1.0).max(0.0) as u32;
    let max_x = (cx + outer_r + 1.0).min(img.width() as f32) as u32;
    let min_y = (cy - outer_r - 1.0).max(0.0) as u32;
    let max_y = (cy + outer_r + 1.0).min(img.height() as f32) as u32;

    for py in min_y..max_y {
        for px in min_x..max_x {
            let dx = px as f32 - cx;
            let dy = py as f32 - cy;
            let distance = (dx * dx + dy * dy).sqrt();

            if distance <= outer_r {
                let bg = img.get_pixel(px, py);

                if distance >= inner_r {
                    // Outline region
                    let mut alpha: f32 = 1.0;

                    // AA for outer edge
                    if distance > outer_r - 1.0 {
                        alpha = alpha.min(outer_r - distance);
                    }
                    // AA for inner edge
                    if distance < inner_r + 1.0 {
                        alpha = alpha.min(distance - inner_r);
                    }

                    if alpha > 0.0 {
                        let blended = blend_colors(*bg, outline_color, alpha);
                        img.put_pixel(px, py, blended);
                    }
                } else {
                    // Fill region
                    let alpha = if distance >= inner_r - 1.0 {
                        (inner_r - distance).clamp(0.0, 1.0)
                    } else {
                        1.0
                    };

                    if alpha > 0.0 {
                        let blended = blend_colors(*bg, fill_color, alpha);
                        img.put_pixel(px, py, blended);
                    }
                }
            }
        }
    }
}

/// Draw only the outline of a rounded rectangle with antialiasing
fn draw_volume_bar_outline(
    img: &mut RgbaImage,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    corner_radius: u32,
    outline_color: Rgba<u8>,
    outline_thickness: u32,
) {
    let x_f = x as f32;
    let y_f = y as f32;
    let width_f = width as f32;
    let height_f = height as f32;
    let r = corner_radius as f32;
    let thickness = outline_thickness as f32;

    let min_x = (x_f - 1.0).max(0.0) as u32;
    let max_x = (x_f + width_f + 1.0).min(img.width() as f32) as u32;
    let min_y = (y_f - 1.0).max(0.0) as u32;
    let max_y = (y_f + height_f + 1.0).min(img.height() as f32) as u32;

    for py in min_y..max_y {
        for px in min_x..max_x {
            let px_f = px as f32;
            let py_f = py as f32;

            let dist_outer = volume_bar_distance(px_f, py_f, x_f, y_f, width_f, height_f, r);

            let inner_x = x_f + thickness;
            let inner_y = y_f + thickness;
            let inner_width = width_f - thickness * 2.0;
            let inner_height = height_f - thickness * 2.0;
            let inner_r = (r - thickness).max(0.0);
            let dist_inner = volume_bar_distance(
                px_f,
                py_f,
                inner_x,
                inner_y,
                inner_width,
                inner_height,
                inner_r,
            );

            if dist_outer <= 0.0 && dist_inner > 0.0 {
                let mut alpha: f32 = 1.0;

                if dist_outer > -1.0 {
                    alpha = alpha.min(-dist_outer);
                }

                if dist_inner < 1.0 {
                    alpha = alpha.min(dist_inner);
                }

                if alpha > 0.0 {
                    let bg = img.get_pixel(px, py);
                    let blended = blend_colors(*bg, outline_color, alpha);
                    img.put_pixel(px, py, blended);
                }
            }
        }
    }
}

/// Get base64 encoded volume bar images for 2 vertical Stream Deck icons
fn get_volume_bar_base64_split(volume_percent: f32) -> Result<(String, String)> {
    let (top_img, bottom_img) = generate_volume_bar_split(volume_percent);

    let mut top_buffer = Vec::new();
    let mut top_cursor = Cursor::new(&mut top_buffer);
    top_img.write_to(&mut top_cursor, image::ImageFormat::Png)?;
    let top_base64 = general_purpose::STANDARD.encode(&top_buffer);

    let mut bottom_buffer = Vec::new();
    let mut bottom_cursor = Cursor::new(&mut bottom_buffer);
    bottom_img.write_to(&mut bottom_cursor, image::ImageFormat::Png)?;
    let bottom_base64 = general_purpose::STANDARD.encode(&bottom_buffer);

    Ok((top_base64, bottom_base64))
}

/// Generate a 100x100 encoder LCD image showing the app title, icon, and volume bar.
///
/// Layout (native canvas, top-left origin):
///   - Title strip: y=0..28, x=0..88, Noto Sans Bold ~22px, centered, truncated to fit
///   - Icon: 64x64 resized from source, centered in the (x=0..88, y=30..94) main area
///   - Volume bar: x=88..98 (10px wide), y=4..96 (92px tall), filled bottom-up
///   - If muted: entire image dimmed to ~35% brightness
pub fn get_encoder_lcd_data_uri(icon_data_uri: &str, title: &str, vol_percent: f32, muted: bool, scroll_offset: f32) -> Result<String> {
    const W: u32 = 100;
    const H: u32 = 100;
    const TEXT_H: u32 = 28;
    const CONTENT_W: u32 = 88; // leave 12px on the right for the volume bar

    let mut img = RgbaImage::from_pixel(W, H, Rgba([18, 18, 18, 255]));

    // --- Title text at top ---
    if scroll_offset > 0.0 {
        let font = TITLE_FONT.as_ref();
        let text_width = font.map(|f| measure_text_width(f, title, PxScale::from(22.0))).unwrap_or(0.0);
        draw_text_scrolling(&mut img, title, 0, 0, CONTENT_W, 22.0, Rgba([220, 220, 220, 255]), scroll_offset, text_width, 30.0);
    } else {
        draw_text_centered(&mut img, title, 0, 0, CONTENT_W, 22.0, Rgba([220, 220, 220, 255]));
    }

    // --- App icon, centered in the main content area below the title ---
    const ICON_SIZE: u32 = 64;
    let icon_area_x = 0u32;
    let icon_area_y = TEXT_H + 2; // 2px gap under title
    let icon_area_w = CONTENT_W;
    let icon_area_h = H - icon_area_y - 4; // 4px bottom padding
    let icon_x_off = icon_area_x as i32 + ((icon_area_w as i32) - (ICON_SIZE as i32)) / 2;
    let icon_y_off = icon_area_y as i32 + ((icon_area_h as i32) - (ICON_SIZE as i32)) / 2;

    let icon_bytes_b64 = icon_data_uri.split_once(',').map(|(_, b)| b).unwrap_or("");
    if let Ok(icon_bytes) = base64::engine::general_purpose::STANDARD.decode(icon_bytes_b64) {
        if let Ok(icon_img) = image::load_from_memory(&icon_bytes) {
            let icon = icon_img.resize(ICON_SIZE, ICON_SIZE, image::imageops::FilterType::Lanczos3);
            let icon_rgba = icon.to_rgba8();
            for (px, py, pixel) in icon_rgba.enumerate_pixels() {
                let x = px as i32 + icon_x_off;
                let y = py as i32 + icon_y_off;
                if x >= 0 && y >= 0 && x < W as i32 && y < H as i32 {
                    let a = pixel[3] as f32 / 255.0;
                    let bg = img.get_pixel(x as u32, y as u32);
                    let r = (pixel[0] as f32 * a + bg[0] as f32 * (1.0 - a)) as u8;
                    let g = (pixel[1] as f32 * a + bg[1] as f32 * (1.0 - a)) as u8;
                    let b = (pixel[2] as f32 * a + bg[2] as f32 * (1.0 - a)) as u8;
                    img.put_pixel(x as u32, y as u32, Rgba([r, g, b, 255]));
                }
            }
        }
    }

    // --- Vertical volume bar on the right ---
    const BAR_X: u32 = 88;
    const BAR_W: u32 = 10;
    const BAR_TOP: u32 = 4;
    const BAR_BOT: u32 = 96;
    let bar_h = BAR_BOT - BAR_TOP;
    let filled_h = (bar_h as f32 * (vol_percent / 100.0).clamp(0.0, 1.0)) as u32;

    // Bar track background
    for y in BAR_TOP..BAR_BOT {
        for x in BAR_X..BAR_X + BAR_W {
            img.put_pixel(x, y, Rgba([40, 40, 40, 255]));
        }
    }

    // Filled portion, bottom-up
    let fill_color = if muted {
        Rgba([90, 90, 90, 255])
    } else {
        Rgba([80, 200, 120, 255])
    };
    let fill_start = BAR_BOT.saturating_sub(filled_h);
    for y in fill_start..BAR_BOT {
        for x in BAR_X..BAR_X + BAR_W {
            img.put_pixel(x, y, fill_color);
        }
    }

    // Highlight at top of fill
    if filled_h > 0 && fill_start < BAR_BOT {
        for x in BAR_X..BAR_X + BAR_W {
            img.put_pixel(x, fill_start, Rgba([160, 255, 200, 255]));
        }
    }

    // --- Mute dim overlay ---
    if muted {
        for pixel in img.pixels_mut() {
            pixel[0] = (pixel[0] as f32 * 0.35) as u8;
            pixel[1] = (pixel[1] as f32 * 0.35) as u8;
            pixel[2] = (pixel[2] as f32 * 0.35) as u8;
        }
    }

    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)?;
    Ok(format!("data:image/png;base64,{}", general_purpose::STANDARD.encode(&buf)))
}
