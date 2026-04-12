use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::audio::audio_system::AppInfo;

/// Persistent cache of MPRIS art data and display name by PID.
struct PidCache {
    art_data: Vec<u8>,
    display_name: String,
}

static CACHE: LazyLock<Mutex<HashMap<u32, PidCache>>> =
    LazyLock::new(|| Mutex::const_new(HashMap::new()));

/// Find all MPRIS art files for a given PID, sorted by counter (ascending).
/// Firefox writes art to ~/.config/mozilla/firefox/firefox-mpris/{PID}_{counter}.png
/// Each playing tab gets its own counter, so multiple files exist for multiple tabs.
fn get_mpris_art_files(pid: u32) -> Vec<Vec<u8>> {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return Vec::new(),
    };
    let mpris_dir = std::path::PathBuf::from(home).join(".config/mozilla/firefox/firefox-mpris");
    if !mpris_dir.exists() {
        return Vec::new();
    }

    let prefix = format!("{}_", pid);
    let mut files: Vec<(u64, std::path::PathBuf)> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&mpris_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(&prefix) && name_str.ends_with(".png") {
                // Extract the counter number for sorting
                let counter_str = &name_str[prefix.len()..name_str.len() - 4];
                if let Ok(counter) = counter_str.parse::<u64>() {
                    files.push((counter, entry.path()));
                }
            }
        }
    }

    // Sort by counter ascending (older tabs have lower counters)
    files.sort_by_key(|(counter, _)| *counter);

    files
        .into_iter()
        .filter_map(|(_, path)| std::fs::read(&path).ok())
        .collect()
}

/// Check if a name is a generic placeholder that shouldn't be displayed.
fn is_generic_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "audiostream"
        || lower == "audio stream"
        || lower == "audio-src"
        || lower == "app_stream"
        || lower == "playback"
}

/// Enrich AppInfo list with MPRIS art data and cached display names.
/// Only applies MPRIS art when a single audio stream exists for a given PID.
/// Uses a persistent cache so data survives channel destruction/recreation.
pub async fn enrich_with_mpris(applications: &mut Vec<AppInfo>) {
    // Group application indices by PID, preserving order (sorted by uid)
    let mut pid_apps: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, app) in applications.iter().enumerate() {
        if let Some(pid) = app.pid {
            pid_apps.entry(pid).or_default().push(i);
        }
    }

    let mut cache = CACHE.lock().await;

    for (&pid, app_indices) in &pid_apps {
        // Sort streams by uid (sink input index) so ordering matches art file counters
        let mut sorted_indices = app_indices.clone();
        sorted_indices.sort_by_key(|&i| applications[i].uid);

        let art_files = get_mpris_art_files(pid);

        if art_files.is_empty() {
            // No art files — use cache for single-stream PIDs
            if sorted_indices.len() == 1 {
                if let Some(cached) = cache.get(&pid) {
                    applications[sorted_indices[0]].mpris_art_data =
                        Some(cached.art_data.clone());
                }
            }
            continue;
        }

        // Match art files to streams: if counts match, pair them 1:1.
        // If fewer art files than streams, give the newest art to ALL streams
        // (Firefox only keeps one art file for MPRIS, so sharing is the best we can do).
        if art_files.len() >= sorted_indices.len() {
            // Enough art for everyone — take from the end (newest files)
            let offset = art_files.len() - sorted_indices.len();
            for (j, &app_idx) in sorted_indices.iter().enumerate() {
                let art = &art_files[offset + j];
                applications[app_idx].mpris_art_data = Some(art.clone());
            }
        } else {
            // Fewer art files than streams — give newest art to all streams
            let newest = art_files.last().unwrap();
            for &app_idx in &sorted_indices {
                applications[app_idx].mpris_art_data = Some(newest.clone());
            }
        }

        // Update cache with newest art for this PID
        if let Some(newest_art) = art_files.last() {
            let display_name = applications[*sorted_indices.last().unwrap()].app_name.clone();
            cache.insert(pid, PidCache {
                art_data: newest_art.clone(),
                display_name,
            });
        }
    }

    for app in applications.iter_mut() {
        let Some(pid) = app.pid else { continue };

        // Only apply generic name fallback for single-stream PIDs.
        // With multiple streams, each has its own name and the per-PID cache
        // would incorrectly give all streams the same name.
        if pid_apps.get(&pid).map(|v| v.len()).unwrap_or(0) == 1 {
            if is_generic_name(&app.app_name) {
                if let Some(cached) = cache.get(&pid) {
                    if !is_generic_name(&cached.display_name) {
                        app.app_name = cached.display_name.clone();
                    }
                }
            } else if let Some(cached) = cache.get_mut(&pid) {
                cached.display_name = app.app_name.clone();
            }
        }
    }

    // Only remove cache entries for PIDs whose process no longer exists
    cache.retain(|&pid, _| {
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    });
}

/// Schedule a delayed full refresh.
/// Waits 2 seconds for MPRIS art files and PulseAudio metadata to stabilize,
/// then re-queries everything so text and icons both update.
pub fn schedule_delayed_refresh() {
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ = crate::audio::pulse::pulse_monitor::refresh_audio_applications().await;
    });
}

