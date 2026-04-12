use std::collections::HashMap;
use std::sync::LazyLock;
use tokio::sync::Mutex;

use ab_glyph::PxScale;
use openaction::get_instance;

use crate::mixer::MIXER_CHANNELS;

const SCROLL_SPEED_PX: f32 = 1.5;
const SCROLL_GAP_PX: f32 = 30.0;
const LCD_FONT_SIZE: f32 = 22.0;
const LCD_MAX_WIDTH: f32 = 84.0; // CONTENT_W (88) minus 4px padding

const HEADER_WINDOW: usize = 8;
const HEADER_GAP: &str = "   ";
const HEADER_CHAR_TICKS: u32 = 8; // advance one char every 8 ticks (~400ms)

struct ScrollEntry {
    text: String,
    // LCD pixel scrolling
    pixel_offset: f32,
    text_width_px: f32,
    needs_scroll_lcd: bool,
    // Header char scrolling
    char_offset: usize,
    char_tick_counter: u32,
    needs_scroll_header: bool,
}

static SCROLL_STATE: LazyLock<Mutex<HashMap<u8, ScrollEntry>>> =
    LazyLock::new(|| Mutex::const_new(HashMap::new()));

/// Check if a channel's LCD text is actively scrolling.
pub async fn is_lcd_scrolling(channel_idx: u8) -> bool {
    let scroll = SCROLL_STATE.lock().await;
    scroll
        .get(&channel_idx)
        .is_some_and(|e| e.needs_scroll_lcd)
}

/// Synchronize scroll state with current mixer channels.
/// Call after mixer channels are updated.
pub async fn sync_scroll_state() {
    let channels = MIXER_CHANNELS.lock().await;
    let mut scroll = SCROLL_STATE.lock().await;

    // Remove entries for channels that no longer exist
    scroll.retain(|k, _| channels.contains_key(k));

    let font = crate::gfx::title_font();
    let scale = PxScale::from(LCD_FONT_SIZE);

    for (&idx, channel) in channels.iter() {
        let display_name = if channel.is_multi_sink_app {
            channel
                .sink_name
                .clone()
                .unwrap_or_else(|| channel.app_name.clone())
        } else {
            channel.app_name.clone()
        };

        let needs_update = match scroll.get(&idx) {
            Some(entry) => entry.text != display_name,
            None => true,
        };

        if needs_update {
            let text_width = font
                .map(|f| crate::gfx::measure_text_width(f, &display_name, scale))
                .unwrap_or(0.0);

            let needs_scroll_lcd = text_width > LCD_MAX_WIDTH;
            let needs_scroll_header = display_name.chars().count() > HEADER_WINDOW;

            scroll.insert(
                idx,
                ScrollEntry {
                    text: display_name,
                    pixel_offset: 0.0,
                    text_width_px: text_width,
                    needs_scroll_lcd,
                    char_offset: 0,
                    char_tick_counter: 0,
                    needs_scroll_header,
                },
            );
        }
    }
}

/// Start the scroll animation timer. Call once during plugin init.
pub fn start_scroll_timer() {
    tokio::spawn(async {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(50));
        loop {
            interval.tick().await;
            scroll_tick().await;
        }
    });
}

/// One tick of scroll animation: advance offsets and re-render scrolling displays.
async fn scroll_tick() {
    // Phase 1: advance offsets, collect channels that need redraw
    let mut lcd_redraws: Vec<(u8, f32, String)> = Vec::new(); // (idx, pixel_offset, text)
    let mut header_redraws: Vec<(u8, String)> = Vec::new(); // (idx, windowed_text)

    {
        let mut scroll = SCROLL_STATE.lock().await;
        for (&idx, entry) in scroll.iter_mut() {
            if entry.needs_scroll_lcd {
                let cycle = entry.text_width_px + SCROLL_GAP_PX;
                entry.pixel_offset += SCROLL_SPEED_PX;
                if entry.pixel_offset >= cycle {
                    entry.pixel_offset -= cycle;
                }
                lcd_redraws.push((idx, entry.pixel_offset, entry.text.clone()));
            }

            if entry.needs_scroll_header {
                entry.char_tick_counter += 1;
                if entry.char_tick_counter >= HEADER_CHAR_TICKS {
                    entry.char_tick_counter = 0;
                    let full: String =
                        format!("{}{}{}", entry.text, HEADER_GAP, entry.text);
                    let char_count = entry.text.chars().count() + HEADER_GAP.len();
                    entry.char_offset = (entry.char_offset + 1) % char_count;

                    let windowed: String = full
                        .chars()
                        .skip(entry.char_offset)
                        .take(HEADER_WINDOW)
                        .collect();
                    header_redraws.push((idx, windowed));
                }
            }
        }
    }

    if lcd_redraws.is_empty() && header_redraws.is_empty() {
        return;
    }

    // Phase 2: read channel data and render
    let channels = MIXER_CHANNELS.lock().await;

    for (idx, pixel_offset, text) in lcd_redraws {
        let Some(channel) = channels.get(&idx) else {
            continue;
        };
        let Some(ref dial_id) = channel.dial_id else {
            continue;
        };
        let icon = if channel.mute {
            &channel.icon_uri_mute
        } else {
            &channel.icon_uri
        };
        if let Ok(uri) = crate::gfx::get_encoder_lcd_data_uri(
            icon,
            &text,
            channel.vol_percent,
            channel.mute,
            pixel_offset,
        ) {
            if let Some(instance) = get_instance(dial_id.clone()).await {
                let _ = instance.set_image(Some(uri), None).await;
            }
        }
    }

    for (idx, windowed_text) in header_redraws {
        let Some(channel) = channels.get(&idx) else {
            continue;
        };
        let Some(ref header_id) = channel.header_id else {
            continue;
        };
        if let Some(instance) = get_instance(header_id.clone()).await {
            let _ = instance.set_title(Some(windowed_text), None).await;
        }
    }
}
