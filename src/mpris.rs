use crate::audio::AppInfo;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

/// Find the newest unclaimed MPRIS art file for a given PID.
/// Firefox writes art to ~/.config/mozilla/firefox/firefox-mpris/{PID}_{counter}.png
/// Since multiple streams share a PID, callers pass the set of paths already claimed
/// by other channels so a new slot never picks up another slot's art.
fn get_mpris_art_for_pid(pid: u32, exclude: &HashSet<PathBuf>) -> Option<(PathBuf, Vec<u8>)> {
    let home = std::env::var("HOME").ok()?;
    let mpris_dir = PathBuf::from(home).join(".config/mozilla/firefox/firefox-mpris");
    if !mpris_dir.exists() {
        return None;
    }

    let prefix = format!("{}_", pid);
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;

    if let Ok(entries) = std::fs::read_dir(&mpris_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(&prefix) && name_str.ends_with(".png") {
                let path = entry.path();
                if exclude.contains(&path) {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
                            newest = Some((modified, path));
                        }
                    }
                }
            }
        }
    }

    let (_, path) = newest?;
    let bytes = std::fs::read(&path).ok()?;
    Some((path, bytes))
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

/// Try to claim unclaimed MPRIS art for a PID.
/// Returns the file path (so the caller can mark it as claimed) and the bytes.
pub fn claim_art(pid: u32, exclude: &HashSet<PathBuf>) -> Option<(PathBuf, Vec<u8>)> {
    get_mpris_art_for_pid(pid, exclude)
}

/// Trailing boilerplate that browsers append to the page <title>. Stripped
/// (case-insensitively) so the header shows just the channel/stream name.
const TITLE_SUFFIXES: &[&str] = &[
    " - Watch Live on Kick",
    " - Kick",
    " - Twitch",
    " - YouTube",
];

/// Strip known site boilerplate suffixes from a page title.
/// "PaymoneyWubby Stream - Watch Live on Kick" -> "PaymoneyWubby Stream"
fn clean_stream_title(title: &str) -> String {
    let trimmed = title.trim();
    for suffix in TITLE_SUFFIXES {
        if trimmed.len() >= suffix.len()
            && trimmed[trimmed.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
        {
            return trimmed[..trimmed.len() - suffix.len()].trim().to_string();
        }
    }
    trimmed.to_string()
}

/// Read `xesam:title` from an MPRIS player's metadata over D-Bus.
async fn mpris_title(conn: &zbus::Connection, bus_name: &str) -> Option<String> {
    use std::collections::HashMap;
    use zbus::zvariant::OwnedValue;

    let props = zbus::fdo::PropertiesProxy::builder(conn)
        .destination(bus_name)
        .ok()?
        .path("/org/mpris/MediaPlayer2")
        .ok()?
        .build()
        .await
        .ok()?;

    let iface = zbus::names::InterfaceName::try_from("org.mpris.MediaPlayer2.Player").ok()?;
    let metadata: OwnedValue = props.get(iface, "Metadata").await.ok()?;
    let dict: HashMap<String, OwnedValue> = metadata.try_into().ok()?;
    String::try_from(dict.get("xesam:title")?.clone()).ok()
}

/// Recover real names for streams whose PulseAudio `media.name` is a generic
/// placeholder (e.g. "AudioStream"). Sites that don't use the W3C Media Session
/// API — Kick.com being the motivating case — leave `media.name` generic, but
/// the browser still publishes the page <title> over MPRIS. We match the MPRIS
/// player to the stream by owner PID and borrow its `xesam:title`.
///
/// Limitation: a browser process exposes a single MPRIS player reflecting its
/// active media session, so two generic streams in the same process would both
/// receive that one title. Streams with a real `media.name` (Twitch, YouTube)
/// are left untouched.
pub async fn enrich_generic_names(apps: &mut [AppInfo]) {
    if !apps
        .iter()
        .any(|a| a.pid.is_some() && is_generic_name(&a.app_name))
    {
        return;
    }

    let Ok(conn) = zbus::Connection::session().await else {
        return;
    };
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else {
        return;
    };
    let Ok(names) = dbus.list_names().await else {
        return;
    };

    // Map each MPRIS player bus name to the PID that owns it.
    let mut player_pids: Vec<(String, u32)> = Vec::new();
    for owned in names {
        let name = owned.to_string();
        if !name.starts_with("org.mpris.MediaPlayer2.") {
            continue;
        }
        let Ok(bus_name) = zbus::names::BusName::try_from(name.as_str()) else {
            continue;
        };
        if let Ok(pid) = dbus.get_connection_unix_process_id(bus_name).await {
            player_pids.push((name, pid));
        }
    }
    if player_pids.is_empty() {
        return;
    }

    for app in apps.iter_mut() {
        let Some(pid) = app.pid else { continue };
        if !is_generic_name(&app.app_name) {
            continue;
        }
        for (name, owner_pid) in &player_pids {
            if *owner_pid != pid {
                continue;
            }
            if let Some(title) = mpris_title(&conn, name).await {
                let cleaned = clean_stream_title(&title);
                if !cleaned.is_empty() && !is_generic_name(&cleaned) {
                    app.app_name = cleaned;
                    break;
                }
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_kick_suffix() {
        assert_eq!(
            clean_stream_title("PaymoneyWubby Stream - Watch Live on Kick"),
            "PaymoneyWubby Stream"
        );
    }

    #[test]
    fn strips_case_insensitively_and_trims() {
        assert_eq!(clean_stream_title("Cool Channel - youtube"), "Cool Channel");
        assert_eq!(clean_stream_title("  Spaced Out - Twitch  "), "Spaced Out");
    }

    #[test]
    fn leaves_unrecognized_titles_untouched() {
        assert_eq!(clean_stream_title("Just A Title"), "Just A Title");
    }

    // Live check against the running session bus. Run with:
    //   cargo test -- --ignored --nocapture live_mpris_titles
    #[tokio::test]
    #[ignore]
    async fn live_mpris_titles() {
        let conn = zbus::Connection::session().await.unwrap();
        let dbus = zbus::fdo::DBusProxy::new(&conn).await.unwrap();
        let names = dbus.list_names().await.unwrap();
        let mut found = 0;
        for owned in names {
            let name = owned.to_string();
            if !name.starts_with("org.mpris.MediaPlayer2.") {
                continue;
            }
            let bus = zbus::names::BusName::try_from(name.as_str()).unwrap();
            let pid = dbus.get_connection_unix_process_id(bus).await.unwrap();
            let title = mpris_title(&conn, &name).await;
            let cleaned = title.as_deref().map(clean_stream_title);
            println!("player={name} pid={pid} title={title:?} cleaned={cleaned:?}");
            found += 1;
        }
        println!("MPRIS players found: {found}");
    }
}
