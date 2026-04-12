use std::time::Duration;

/// Find the newest MPRIS art file for a given PID.
/// Firefox writes art to ~/.config/mozilla/firefox/firefox-mpris/{PID}_{counter}.png
fn get_mpris_art_for_pid(pid: u32) -> Option<Vec<u8>> {
    let home = std::env::var("HOME").ok()?;
    let mpris_dir = std::path::PathBuf::from(home).join(".config/mozilla/firefox/firefox-mpris");
    if !mpris_dir.exists() {
        return None;
    }

    let prefix = format!("{}_", pid);
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;

    if let Ok(entries) = std::fs::read_dir(&mpris_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(&prefix) && name_str.ends_with(".png") {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
                            newest = Some((modified, entry.path()));
                        }
                    }
                }
            }
        }
    }

    let (_, path) = newest?;
    std::fs::read(&path).ok()
}

/// Check if a name is a generic placeholder that shouldn't be displayed.
pub fn is_generic_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "audiostream"
        || lower == "audio stream"
        || lower == "audio-src"
        || lower == "app_stream"
        || lower == "playback"
}

/// Try to get MPRIS art for a PID. Returns the art bytes if available.
pub fn get_art(pid: u32) -> Option<Vec<u8>> {
    get_mpris_art_for_pid(pid)
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
