use crate::utils::get_app_icon_uri;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
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
    /// Filesystem path of the MPRIS art file this channel has claimed.
    /// Other channels exclude this path so they never inherit this slot's art.
    pub mpris_art_path: Option<PathBuf>,
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
    let mut claimed_paths: HashSet<PathBuf> = HashSet::new();

    let mut col_key = 0;
    for app in applications.into_iter() {
        if ignored_apps.contains(&app.app_name) {
            println!("Skipping ignored app: {}", app.app_name);
            continue;
        }

        // Try to claim an unclaimed MPRIS art file for this stream.
        let (art_data, art_path) = match app.pid.and_then(|pid| crate::mpris::claim_art(pid, &claimed_paths)) {
            Some((path, bytes)) => {
                claimed_paths.insert(path.clone());
                (Some(bytes), Some(path))
            }
            None => (None, None),
        };

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
                mpris_art_path: art_path,
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

    // Index previous channels by UID so we can carry over name, icon, and art claims.
    let mut previous_by_uid: HashMap<u32, MixerChannel> =
        channels.drain().map(|(_, ch)| (ch.uid, ch)).collect();

    // Pre-seed the claim set with paths owned by streams that are still active.
    // Doing this up front guarantees that a slot without art cannot accidentally
    // re-claim a path already owned by another active slot, regardless of iteration order.
    let active_uids: HashSet<u32> = active_apps.iter().map(|a| a.uid).collect();
    let mut claimed_paths: HashSet<PathBuf> = previous_by_uid
        .iter()
        .filter(|(uid, _)| active_uids.contains(uid))
        .filter_map(|(_, ch)| ch.mpris_art_path.clone())
        .collect();

    let mut new_channels: HashMap<u8, MixerChannel> = HashMap::new();
    let mut col_key: u8 = 0;

    for app in active_apps {
        let channel = if let Some(mut prev) = previous_by_uid.remove(&app.uid) {
            // Detect a media.name shift on the same sink-input (e.g. FFZ's
            // long-lived AudioContext keeps the same uid across Twitch
            // navigations). When this happens, unlock the slot, drop stale
            // art, and reset to a default icon. Re-claim is deferred to the
            // delayed refresh — the new MPRIS art file may not be written
            // yet at this exact moment.
            let name_shifted = !crate::mpris::is_generic_name(&app.app_name)
                && app.app_name != prev.app_name;

            if name_shifted {
                prev.locked = false;
                prev.app_name = app.app_name.clone();
                if let Some(old_path) = prev.mpris_art_path.take() {
                    claimed_paths.remove(&old_path);
                }
                prev.mpris_art_data = None;
                let (icon_uri, icon_uri_mute, uses_default_icon) = get_app_icon_uri(
                    app.icon_name.clone(),
                    app.icon_search_name.clone(),
                    None,
                );
                prev.icon_uri = icon_uri;
                prev.icon_uri_mute = icon_uri_mute;
                prev.uses_default_icon = uses_default_icon;
            } else if prev.mpris_art_path.is_none() {
                // Existing stream that has never had art: try to claim some now.
                if let Some(pid) = app.pid {
                    if let Some((path, bytes)) = crate::mpris::claim_art(pid, &claimed_paths) {
                        claimed_paths.insert(path.clone());
                        let (icon_uri, icon_uri_mute, uses_default_icon) = get_app_icon_uri(
                            app.icon_name.clone(),
                            app.icon_search_name.clone(),
                            Some(&bytes),
                        );
                        prev.icon_uri = icon_uri;
                        prev.icon_uri_mute = icon_uri_mute;
                        prev.uses_default_icon = uses_default_icon;
                        prev.mpris_art_data = Some(bytes);
                        prev.mpris_art_path = Some(path);
                    }
                }
            }

            // Update live data
            prev.mute = app.mute;
            prev.vol_percent = app.vol_percent;
            prev.sink_name = app.sink_name;
            prev.is_device = app.is_device;
            prev.is_multi_sink_app = app.is_multi_sink_app;

            // Upgrade a generic name if a better one is now available, then lock.
            if !prev.locked {
                if !crate::mpris::is_generic_name(&app.app_name) {
                    prev.app_name = app.app_name;
                }
                let has_good_name = !crate::mpris::is_generic_name(&prev.app_name);
                prev.locked = has_good_name
                    && (prev.mpris_art_path.is_some() || app.pid.is_none());
            }

            // Clear position-specific IDs (will be reassigned by update_stream_deck_buttons)
            prev.header_id = None;
            prev.upper_vol_btn_id = None;
            prev.lower_vol_btn_id = None;
            prev.dial_id = None;
            prev
        } else {
            // Brand-new stream. Don't claim MPRIS art immediately — Firefox may
            // not have written the art file for this stream yet (the file on disk
            // could still be from the previous stream). The delayed refresh (2s
            // later) will make a claim against the updated filesystem state.
            // For generic names, borrow a better name from an existing slot on the same PID.
            let display_name = if crate::mpris::is_generic_name(&app.app_name) {
                if let Some(pid) = app.pid {
                    previous_by_uid
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
            let locked = has_good_name && app.pid.is_none();

            let (icon_uri, icon_uri_mute, uses_default_icon) =
                get_app_icon_uri(app.icon_name, app.icon_search_name.clone(), None);

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
                mpris_art_data: None,
                mpris_art_path: None,
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
