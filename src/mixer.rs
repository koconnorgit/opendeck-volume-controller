use crate::audio::audio_system::AppInfo;
use crate::utils::get_app_icon_uri;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::SystemTime;
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct MixerChannel {
    pub header_id: Option<String>,
    pub upper_vol_btn_id: Option<String>,
    pub lower_vol_btn_id: Option<String>,
    pub dial_id: Option<String>,
    pub uid: u32,
    /// All sink-input indices this channel controls (collapsed multi-stream apps
    /// list every member here; a normal channel just holds `[uid]`). Volume and
    /// mute are applied to every member.
    pub member_uids: Vec<u32>,
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
    /// Wall-clock time this sink-input was first observed appearing, used to
    /// correlate art files to the stream by timing. `None` means the stream was
    /// already playing at cold start (true start unknown → best-effort claim).
    pub first_seen: Option<std::time::SystemTime>,
    /// Once true, the art-claim decision is final and never re-attempted —
    /// whether it produced art or a deliberate "no art". Stops a later refresh
    /// from grabbing an unrelated file. Reset only on a name-shift.
    pub art_resolved: bool,
    pub is_device: bool,
    pub is_multi_sink_app: bool,
}

pub static MIXER_CHANNELS: LazyLock<Mutex<HashMap<u8, MixerChannel>>> =
    LazyLock::new(|| Mutex::const_new(HashMap::new()));

/// Maps encoder dial position → mixer channel index (independent from button columns)
pub static ENCODER_TO_CHANNEL_MAP: LazyLock<Mutex<HashMap<u8, u8>>> =
    LazyLock::new(|| Mutex::const_new(HashMap::new()));

/// Count how many active streams share each PID, so the cold-start art claim can
/// tell an unambiguous single-stream PID from an ambiguous multi-stream one.
fn count_pids(apps: &[AppInfo]) -> HashMap<u32, usize> {
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for app in apps {
        if let Some(pid) = app.pid {
            *counts.entry(pid).or_insert(0) += 1;
        }
    }
    counts
}

/// Apply a claimed art file to a channel: mark the path claimed and rebuild the
/// icon (normal + muted) from the art bytes.
fn apply_art(
    channel: &mut MixerChannel,
    icon_name: Option<String>,
    icon_search_name: String,
    path: PathBuf,
    bytes: Vec<u8>,
    claimed_paths: &mut HashSet<PathBuf>,
) {
    claimed_paths.insert(path.clone());
    let (icon_uri, icon_uri_mute, uses_default_icon) =
        get_app_icon_uri(icon_name, icon_search_name, Some(&bytes), None, None);
    channel.icon_uri = icon_uri;
    channel.icon_uri_mute = icon_uri_mute;
    channel.uses_default_icon = uses_default_icon;
    channel.mpris_art_data = Some(bytes);
    channel.mpris_art_path = Some(path);
}

/// Make (or defer) the art-claim decision for a channel. No-op once resolved.
///
/// For an observed stream (`first_seen = Some`) it waits `SETTLE` for the art to
/// be written, then commits a final decision either way. At cold start it
/// best-effort claims for unambiguous single-stream PIDs and otherwise leaves
/// the channel unresolved so a later refresh can try again once the art lands.
fn resolve_art(
    channel: &mut MixerChannel,
    icon_name: Option<String>,
    icon_search_name: String,
    now: SystemTime,
    pid_counts: &HashMap<u32, usize>,
    claimed_paths: &mut HashSet<PathBuf>,
) {
    if channel.art_resolved {
        return;
    }
    let Some(pid) = channel.pid else {
        // Devices / system audio never have Firefox art; decision is immediate.
        channel.art_resolved = true;
        return;
    };
    let same_pid_count = pid_counts.get(&pid).copied().unwrap_or(1);

    match channel.first_seen {
        Some(first_seen) => {
            // Too early: the browser may not have written art yet. Retry later.
            let too_early = now
                .duration_since(first_seen)
                .map(|age| age < crate::mpris::SETTLE)
                .unwrap_or(false);
            if too_early {
                return;
            }
            if let Some((path, bytes)) =
                crate::mpris::claim_art(pid, Some(first_seen), same_pid_count, claimed_paths)
            {
                apply_art(channel, icon_name, icon_search_name, path, bytes, claimed_paths);
            }
            // Final whether or not a file matched.
            channel.art_resolved = true;
        }
        None => {
            // Cold start. Refuse to guess when the PID is shared.
            if same_pid_count > 1 {
                channel.art_resolved = true;
                return;
            }
            if let Some((path, bytes)) =
                crate::mpris::claim_art(pid, None, same_pid_count, claimed_paths)
            {
                apply_art(channel, icon_name, icon_search_name, path, bytes, claimed_paths);
                channel.art_resolved = true;
            }
            // No art on disk yet → stay unresolved; a later refresh can claim it.
        }
    }
}

pub async fn create_mixer_channels(applications: Vec<AppInfo>, ignored_apps: &[String]) {
    let mut channels = MIXER_CHANNELS.lock().await;
    let mut claimed_paths: HashSet<PathBuf> = HashSet::new();
    let now = SystemTime::now();

    let mut active: Vec<AppInfo> = Vec::new();
    for app in applications.into_iter() {
        if ignored_apps.contains(&app.app_name) {
            println!("Skipping ignored app: {}", app.app_name);
            continue;
        }
        active.push(app);
    }
    let pid_counts = count_pids(&active);

    let mut col_key: u8 = 0;
    for app in active.into_iter() {
        let (icon_uri, icon_uri_mute, uses_default_icon) = get_app_icon_uri(
            app.icon_name.clone(),
            app.icon_search_name.clone(),
            None,
            app.wm_class.as_deref(),
            app.window_icon.as_ref(),
        );

        let mut channel = MixerChannel {
            header_id: None,
            upper_vol_btn_id: None,
            lower_vol_btn_id: None,
            dial_id: None,
            uid: app.uid,
            member_uids: app.member_uids.clone(),
            pid: app.pid,
            app_name: app.app_name.clone(),
            sink_name: app.sink_name.clone(),
            mute: app.mute,
            vol_percent: app.vol_percent,
            icon_uri,
            icon_uri_mute,
            uses_default_icon,
            mpris_art_data: None,
            mpris_art_path: None,
            // Cold start: these streams may have been playing for a while, so we
            // have no reliable timing anchor for them.
            first_seen: None,
            art_resolved: false,
            locked: false,
            is_device: app.is_device,
            is_multi_sink_app: app.is_multi_sink_app,
        };

        resolve_art(
            &mut channel,
            app.icon_name,
            app.icon_search_name,
            now,
            &pid_counts,
            &mut claimed_paths,
        );

        let has_good_name = !crate::mpris::is_generic_name(&channel.app_name);
        channel.locked = has_good_name && channel.art_resolved;

        channels.insert(col_key, channel);
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

    let now = SystemTime::now();
    let pid_counts = count_pids(&active_apps);

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
                // New content is a fresh stream as far as art goes: re-anchor the
                // timing and re-open the claim decision so the next refresh can
                // bind the new art file by timing.
                prev.first_seen = Some(now);
                prev.art_resolved = false;
                let (icon_uri, icon_uri_mute, uses_default_icon) = get_app_icon_uri(
                    app.icon_name.clone(),
                    app.icon_search_name.clone(),
                    None,
                    app.wm_class.as_deref(),
                    app.window_icon.as_ref(),
                );
                prev.icon_uri = icon_uri;
                prev.icon_uri_mute = icon_uri_mute;
                prev.uses_default_icon = uses_default_icon;
            }

            // Update live data
            prev.mute = app.mute;
            prev.vol_percent = app.vol_percent;
            prev.sink_name = app.sink_name;
            prev.is_device = app.is_device;
            prev.is_multi_sink_app = app.is_multi_sink_app;
            prev.member_uids = app.member_uids.clone();

            // Make (or defer) the timing-aware art decision. No-op once resolved.
            resolve_art(
                &mut prev,
                app.icon_name.clone(),
                app.icon_search_name.clone(),
                now,
                &pid_counts,
                &mut claimed_paths,
            );

            // Upgrade a generic name if a better one is now available, then lock.
            if !prev.locked {
                if !crate::mpris::is_generic_name(&app.app_name) {
                    prev.app_name = app.app_name;
                }
                let has_good_name = !crate::mpris::is_generic_name(&prev.app_name);
                prev.locked = has_good_name && prev.art_resolved;
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

            // Anchor timing for streams with a PID (browser tabs); devices /
            // system audio have no PID and never carry Firefox art, so their art
            // decision is already final.
            let (first_seen, art_resolved) = if app.pid.is_some() {
                (Some(now), false)
            } else {
                (None, true)
            };

            let has_good_name = !crate::mpris::is_generic_name(&display_name);
            let locked = has_good_name && art_resolved;

            let (icon_uri, icon_uri_mute, uses_default_icon) = get_app_icon_uri(
                app.icon_name.clone(),
                app.icon_search_name.clone(),
                None,
                app.wm_class.as_deref(),
                app.window_icon.as_ref(),
            );

            MixerChannel {
                header_id: None,
                upper_vol_btn_id: None,
                lower_vol_btn_id: None,
                dial_id: None,
                uid: app.uid,
                member_uids: app.member_uids.clone(),
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
                first_seen,
                art_resolved,
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
