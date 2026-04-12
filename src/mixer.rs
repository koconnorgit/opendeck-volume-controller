use crate::utils::get_app_icon_uri;
use std::collections::HashMap;
use std::sync::LazyLock;
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct MixerChannel {
    pub header_id: Option<String>,
    pub upper_vol_btn_id: Option<String>,
    pub lower_vol_btn_id: Option<String>,
    pub dial_id: Option<String>,
    pub uid: u32,
    pub pid: Option<u32>,
    pub app_name: String,
    pub sink_name: Option<String>,
    pub mute: bool,
    pub vol_percent: f32,
    pub icon_uri: String,
    pub icon_uri_mute: String,
    pub uses_default_icon: bool,
    /// Cached MPRIS art image bytes — locked once captured.
    pub mpris_art_data: Option<Vec<u8>>,
    /// Once true, name and icon are locked and won't be overwritten by refreshes.
    /// Only volume, mute, and other live data update. Reset when stream stops.
    pub locked: bool,
    pub is_device: bool,
    pub is_multi_sink_app: bool,
}

pub static MIXER_CHANNELS: LazyLock<Mutex<HashMap<u8, MixerChannel>>> =
    LazyLock::new(|| Mutex::const_new(HashMap::new()));

/// Maps encoder dial position → mixer channel index (independent from button columns)
pub static ENCODER_TO_CHANNEL_MAP: LazyLock<Mutex<HashMap<u8, u8>>> =
    LazyLock::new(|| Mutex::const_new(HashMap::new()));

pub async fn create_mixer_channels(
    applications: Vec<crate::audio::audio_system::AppInfo>,
    ignored_apps: &[String],
) {
    let mut channels = MIXER_CHANNELS.lock().await;

    let mut col_key = 0;
    for app in applications.into_iter() {
        if ignored_apps.contains(&app.app_name) {
            println!("Skipping ignored app: {}", app.app_name);
            continue;
        }

        // Try to get MPRIS art for this stream
        let art_data = app.pid.and_then(crate::mpris::get_art);
        let has_good_name = !crate::mpris::is_generic_name(&app.app_name);
        let locked = has_good_name && (art_data.is_some() || app.pid.is_none());

        let (icon_uri, icon_uri_mute, uses_default_icon) =
            get_app_icon_uri(app.icon_name, app.icon_search_name.clone(), art_data.as_deref());

        channels.insert(
            col_key as u8,
            MixerChannel {
                header_id: None,
                upper_vol_btn_id: None,
                lower_vol_btn_id: None,
                dial_id: None,
                uid: app.uid,
                pid: app.pid,
                app_name: app.app_name.clone(),
                sink_name: app.sink_name.clone(),
                mute: app.mute,
                vol_percent: app.vol_percent,
                icon_uri,
                icon_uri_mute,
                uses_default_icon,
                mpris_art_data: art_data,
                locked,
                is_device: app.is_device,
                is_multi_sink_app: app.is_multi_sink_app,
            },
        );

        col_key += 1;
    }
}

pub async fn update_mixer_channels(
    applications: Vec<crate::audio::audio_system::AppInfo>,
    ignored_apps: &[String],
) {
    let mut channels = MIXER_CHANNELS.lock().await;

    // Build list of active apps (filtered)
    let active_apps: Vec<_> = applications
        .into_iter()
        .filter(|app| !ignored_apps.contains(&app.app_name))
        .collect();

    // Track all previously known UIDs so we can distinguish truly new streams
    let previous_uids: std::collections::HashSet<u32> =
        channels.values().map(|ch| ch.uid).collect();

    // Save locked channel data by UID before rebuilding positions.
    // This preserves locked name/icon even when positions shift.
    let mut locked_data: HashMap<u32, MixerChannel> = HashMap::new();
    for channel in channels.values() {
        if channel.locked {
            locked_data.insert(channel.uid, channel.clone());
        }
    }

    // Rebuild channels in the order apps appear
    let mut new_channels: HashMap<u8, MixerChannel> = HashMap::new();
    let mut col_key: u8 = 0;

    for app in active_apps {
        let channel = if let Some(mut locked) = locked_data.remove(&app.uid) {
            // This stream was previously locked — keep its name and icon,
            // just update live data
            locked.mute = app.mute;
            locked.vol_percent = app.vol_percent;
            locked.sink_name = app.sink_name;
            locked.is_device = app.is_device;
            locked.is_multi_sink_app = app.is_multi_sink_app;
            // Clear position-specific IDs (will be reassigned by update_stream_deck_buttons)
            locked.header_id = None;
            locked.upper_vol_btn_id = None;
            locked.lower_vol_btn_id = None;
            locked.dial_id = None;
            locked
        } else {
            // New or unlocked stream — capture data.
            // DON'T read MPRIS art immediately for new streams — Firefox may not
            // have written the art file for this stream yet (the file on disk
            // is still from the previous stream). The delayed refresh (2s later)
            // will capture the correct art once Firefox has written it.
            let is_new_stream = !previous_uids.contains(&app.uid);
            let art_data = if is_new_stream {
                None // Let the delayed refresh capture art
            } else {
                app.pid.and_then(crate::mpris::get_art)
            };

            // For generic names, try per-PID fallback from locked channels
            let display_name = if crate::mpris::is_generic_name(&app.app_name) {
                if let Some(pid) = app.pid {
                    locked_data
                        .values()
                        .chain(new_channels.values())
                        .find(|ch| {
                            ch.pid == Some(pid)
                                && !crate::mpris::is_generic_name(&ch.app_name)
                        })
                        .map(|ch| ch.app_name.clone())
                        .unwrap_or(app.app_name.clone())
                } else {
                    app.app_name.clone()
                }
            } else {
                app.app_name.clone()
            };

            let has_good_name = !crate::mpris::is_generic_name(&display_name);
            let locked = has_good_name && (art_data.is_some() || app.pid.is_none());

            let (icon_uri, icon_uri_mute, uses_default_icon) =
                get_app_icon_uri(app.icon_name, app.icon_search_name.clone(), art_data.as_deref());

            MixerChannel {
                header_id: None,
                upper_vol_btn_id: None,
                lower_vol_btn_id: None,
                dial_id: None,
                uid: app.uid,
                pid: app.pid,
                app_name: display_name,
                sink_name: app.sink_name,
                mute: app.mute,
                vol_percent: app.vol_percent,
                icon_uri,
                icon_uri_mute,
                uses_default_icon,
                mpris_art_data: art_data,
                locked,
                is_device: app.is_device,
                is_multi_sink_app: app.is_multi_sink_app,
            }
        };

        new_channels.insert(col_key, channel);
        col_key += 1;
    }

    *channels = new_channels;

    println!(
        "Updated mixer channels (filtered {} ignored apps)",
        ignored_apps.len()
    );
}
