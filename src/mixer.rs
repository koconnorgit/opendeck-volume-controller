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
    /// Stable app identity used for exclude-list matching (see `AppInfo::app_id`).
    pub app_id: String,
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

impl MixerChannel {
    /// Text shown on the LCD / header for this channel. Collapsed multi-sink apps
    /// show the sink name; everything else shows the stream name, with Kick tab
    /// titles trimmed to just the streamer handle. The full
    /// `"<handle> Stream - Watch Live on Kick"` is kept in `app_name` (the avatar
    /// fetch and Kick detection key off it) but is too long/noisy to display.
    pub fn display_label(&self) -> String {
        if self.is_multi_sink_app {
            self.sink_name.clone().unwrap_or_else(|| self.app_name.clone())
        } else {
            crate::kick::display_name(&self.app_name)
        }
    }
}

pub static MIXER_CHANNELS: LazyLock<Mutex<HashMap<u8, MixerChannel>>> =
    LazyLock::new(|| Mutex::const_new(HashMap::new()));

/// Maps encoder dial position → mixer channel index (independent from button columns)
pub static ENCODER_TO_CHANNEL_MAP: LazyLock<Mutex<HashMap<u8, u8>>> =
    LazyLock::new(|| Mutex::const_new(HashMap::new()));

/// How long a logical stream's art is remembered after the sink-input it was
/// attached to disappears. Long enough to bridge a stream being torn down and
/// recreated under a new sink-input index (e.g. a browser re-routing a `<video>`
/// through the Web Audio API for a compressor/EQ), short enough that art is not
/// mis-applied to an unrelated later stream that happens to reuse the identity.
const ART_MEMORY_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// MPRIS art remembered by *logical* stream identity (PID + display name) rather
/// than the volatile sink-input index that drives `MixerChannel::uid`. When a
/// stream is recreated under a new index its successor can inherit this art
/// instead of reverting to a generic app icon. See `update_mixer_channels`.
struct RememberedArt {
    path: PathBuf,
    data: Vec<u8>,
    last_seen: SystemTime,
}

static ART_MEMORY: LazyLock<std::sync::Mutex<HashMap<(u32, String), RememberedArt>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Record a channel's claimed art under its logical identity, if it has both a
/// PID and a real (non-generic) name. No-op for channels without art.
fn remember_channel_art(
    mem: &mut HashMap<(u32, String), RememberedArt>,
    ch: &MixerChannel,
    now: SystemTime,
) {
    if crate::mpris::is_generic_name(&ch.app_name) {
        return;
    }
    let (Some(pid), Some(path), Some(data)) =
        (ch.pid, ch.mpris_art_path.clone(), ch.mpris_art_data.clone())
    else {
        return;
    };
    mem.insert(
        (pid, ch.app_name.clone()),
        RememberedArt {
            path,
            data,
            last_seen: now,
        },
    );
}

/// Count how many active streams share each PID, so the cold-start art claim can
/// tell an unambiguous single-stream PID from an ambiguous multi-stream one.
fn count_pids(apps: &[AppInfo]) -> HashMap<u32, usize> {
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for app in apps {
        // Kick streams never write a Firefox MPRIS art file, so they don't make a
        // shared PID ambiguous for a sibling tab's cold-start claim. Counting them
        // would wrongly block e.g. a Twitch tab from claiming its art when a Kick
        // tab is open in the same browser process.
        if crate::kick::kick_slug(&app.app_name).is_some() {
            continue;
        }
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

/// Whether an app is on the user's exclude list. Matches the stable app id
/// (so an app stays excluded across track/tab/title changes) and, for backward
/// compatibility, the display name (older entries were stored that way).
fn is_ignored(app: &AppInfo, ignored_apps: &[String]) -> bool {
    ignored_apps.iter().any(|n| n == &app.app_id || n == &app.app_name)
}

pub async fn create_mixer_channels(applications: Vec<AppInfo>, ignored_apps: &[String]) {
    let mut channels = MIXER_CHANNELS.lock().await;
    let mut claimed_paths: HashSet<PathBuf> = HashSet::new();
    let now = SystemTime::now();

    let mut active: Vec<AppInfo> = Vec::new();
    for app in applications.into_iter() {
        if is_ignored(&app, ignored_apps) {
            println!("Skipping ignored app: {}", app.app_name);
            continue;
        }
        active.push(app);
    }
    let pid_counts = count_pids(&active);

    let mut col_key: u8 = 0;
    for app in active.into_iter() {
        // A Kick avatar (when already fetched) takes the MPRIS-art slot.
        let (icon_uri, icon_uri_mute, uses_default_icon) = get_app_icon_uri(
            app.icon_name.clone(),
            app.icon_search_name.clone(),
            app.kick_art.as_deref(),
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
            app_id: app.app_id.clone(),
            sink_name: app.sink_name.clone(),
            mute: app.mute,
            vol_percent: app.vol_percent,
            icon_uri,
            icon_uri_mute,
            uses_default_icon,
            mpris_art_data: app.kick_art.clone(),
            mpris_art_path: None,
            // Cold start: these streams may have been playing for a while, so we
            // have no reliable timing anchor for them.
            first_seen: None,
            art_resolved: false,
            locked: false,
            is_device: app.is_device,
            is_multi_sink_app: app.is_multi_sink_app,
        };

        if crate::kick::kick_slug(&app.app_name).is_some() {
            // Kick streams never write their own MPRIS art file; their only art
            // source is the avatar (already in `mpris_art_data` above, if fetched).
            // Skip the file-claim so a sibling tab's art can't be mis-attributed.
            channel.art_resolved = true;
        } else {
            resolve_art(
                &mut channel,
                app.icon_name,
                app.icon_search_name,
                now,
                &pid_counts,
                &mut claimed_paths,
            );
        }

        let has_good_name = !crate::mpris::is_generic_name(&channel.app_name);
        // Keep a Kick channel unlocked until its avatar fetch settles, so a later
        // refresh can swap in the avatar instead of locking the default icon.
        channel.locked = has_good_name && channel.art_resolved && !app.kick_pending;

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
        .filter(|app| !is_ignored(app, ignored_apps))
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

    // Remember every prior channel's art by logical identity *before* processing.
    // This deliberately captures streams that are about to disappear this refresh
    // (their uid is gone from the active set): a recreated stream's successor,
    // processed below, inherits exactly that just-departed predecessor's art.
    // Pruning here also bounds the table to the grace window.
    {
        let mut mem = ART_MEMORY.lock().unwrap();
        mem.retain(|_, a| {
            now.duration_since(a.last_seen)
                .map(|age| age < ART_MEMORY_TTL)
                .unwrap_or(true)
        });
        for ch in previous_by_uid.values() {
            remember_channel_art(&mut mem, ch, now);
        }
    }

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
            prev.app_id = app.app_id.clone();

            if crate::kick::kick_slug(&app.app_name).is_some() {
                // Kick streams never write their own MPRIS art file. Skip the
                // file-claim (so a sibling tab's art can't be mis-attributed) and
                // instead drop in the avatar once the fetch lands.
                prev.art_resolved = true;
                if let Some(art) = app.kick_art.as_deref().filter(|_| prev.mpris_art_data.is_none())
                {
                    let (icon_uri, icon_uri_mute, uses_default_icon) = get_app_icon_uri(
                        app.icon_name.clone(),
                        app.icon_search_name.clone(),
                        Some(art),
                        None,
                        None,
                    );
                    prev.icon_uri = icon_uri;
                    prev.icon_uri_mute = icon_uri_mute;
                    prev.uses_default_icon = uses_default_icon;
                    prev.mpris_art_data = Some(art.to_vec());
                }
            } else {
                // Make (or defer) the timing-aware art decision. No-op once resolved.
                resolve_art(
                    &mut prev,
                    app.icon_name.clone(),
                    app.icon_search_name.clone(),
                    now,
                    &pid_counts,
                    &mut claimed_paths,
                );
            }

            // Upgrade a generic name if a better one is now available, then lock.
            if !prev.locked {
                if !crate::mpris::is_generic_name(&app.app_name) {
                    prev.app_name = app.app_name;
                }
                let has_good_name = !crate::mpris::is_generic_name(&prev.app_name);
                // Stay unlocked while a Kick avatar fetch is still in flight.
                prev.locked = has_good_name && prev.art_resolved && !app.kick_pending;
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

            // Before treating this as a cold stream, check whether it's really the
            // successor of a just-departed (or very recently seen) stream with the
            // same logical identity (PID + display name) — e.g. a browser that
            // re-routed its audio through the Web Audio API, replacing the
            // sink-input under a new index. `uid` can't catch this because it *is*
            // the index; the identity-keyed memory can. Inheriting art skips the
            // generic-icon flash and the timing-window reclaim entirely. Never
            // steal art a still-active sibling already holds this refresh.
            let inherited_art = if !crate::mpris::is_generic_name(&display_name) {
                app.pid.and_then(|pid| {
                    let mem = ART_MEMORY.lock().unwrap();
                    mem.get(&(pid, display_name.clone())).and_then(|a| {
                        if claimed_paths.contains(&a.path) {
                            None
                        } else {
                            Some((a.path.clone(), a.data.clone()))
                        }
                    })
                })
            } else {
                None
            };

            // With inherited art the decision is final; otherwise anchor timing for
            // streams with a PID (browser tabs) so the delayed refresh can claim a
            // freshly written art file. Devices / system audio have no PID and never
            // carry Firefox art, so their art decision is already final.
            let (
                icon_uri,
                icon_uri_mute,
                uses_default_icon,
                mpris_art_data,
                mpris_art_path,
                first_seen,
                art_resolved,
            ) = if let Some((path, data)) = inherited_art {
                claimed_paths.insert(path.clone());
                let (icon_uri, icon_uri_mute, uses_default_icon) = get_app_icon_uri(
                    app.icon_name.clone(),
                    app.icon_search_name.clone(),
                    Some(&data),
                    None,
                    None,
                );
                (
                    icon_uri,
                    icon_uri_mute,
                    uses_default_icon,
                    Some(data),
                    Some(path),
                    Some(now),
                    true,
                )
            } else {
                let (first_seen, art_resolved) = if app.pid.is_some() {
                    (Some(now), false)
                } else {
                    (None, true)
                };
                // A ready Kick avatar takes the MPRIS-art slot for this new stream.
                let (icon_uri, icon_uri_mute, uses_default_icon) = get_app_icon_uri(
                    app.icon_name.clone(),
                    app.icon_search_name.clone(),
                    app.kick_art.as_deref(),
                    app.wm_class.as_deref(),
                    app.window_icon.as_ref(),
                );
                (
                    icon_uri,
                    icon_uri_mute,
                    uses_default_icon,
                    app.kick_art.clone(),
                    None,
                    first_seen,
                    art_resolved,
                )
            };

            let has_good_name = !crate::mpris::is_generic_name(&display_name);
            // Stay unlocked while a Kick avatar fetch is still in flight.
            let locked = has_good_name && art_resolved && !app.kick_pending;

            MixerChannel {
                header_id: None,
                upper_vol_btn_id: None,
                lower_vol_btn_id: None,
                dial_id: None,
                uid: app.uid,
                member_uids: app.member_uids.clone(),
                pid: app.pid,
                app_name: display_name,
                app_id: app.app_id.clone(),
                sink_name: app.sink_name,
                mute: app.mute,
                vol_percent: app.vol_percent,
                icon_uri,
                icon_uri_mute,
                uses_default_icon,
                mpris_art_data,
                mpris_art_path,
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

    // Refresh the identity memory with every currently-live channel's art so its
    // grace window is measured from now. This keeps art for a still-playing stream
    // alive indefinitely and only starts the TTL countdown once it truly stops.
    {
        let mut mem = ART_MEMORY.lock().unwrap();
        for ch in new_channels.values() {
            remember_channel_art(&mut mem, ch, now);
        }
    }

    *channels = new_channels;

    println!(
        "Updated mixer channels (filtered {} ignored apps)",
        ignored_apps.len()
    );
}
