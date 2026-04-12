use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::{Mutex, OnceCell};
use zbus::Connection;
use zbus::zvariant::{OwnedValue, Value};

use crate::audio::audio_system::AppInfo;

static DBUS_CONNECTION: OnceCell<Connection> = OnceCell::const_new();

/// Persistent cache of MPRIS art data and display name by PID.
/// Survives channel destruction/recreation and timing gaps.
struct PidCache {
    art_data: Vec<u8>,
    display_name: String,
}

static CACHE: LazyLock<Mutex<HashMap<u32, PidCache>>> =
    LazyLock::new(|| Mutex::const_new(HashMap::new()));

async fn get_connection() -> Option<&'static Connection> {
    DBUS_CONNECTION
        .get_or_try_init(|| Connection::session())
        .await
        .ok()
}

/// Query MPRIS D-Bus services to find media art for a process with the given PID.
async fn get_mpris_art(pid: u32) -> Option<Vec<u8>> {
    tokio::time::timeout(Duration::from_millis(500), get_mpris_art_inner(pid))
        .await
        .ok()?
}

async fn get_mpris_art_inner(pid: u32) -> Option<Vec<u8>> {
    let conn = get_connection().await?;
    let dbus = zbus::fdo::DBusProxy::new(conn).await.ok()?;

    let names = dbus.list_names().await.ok()?;
    let mpris_names: Vec<_> = names
        .into_iter()
        .filter(|n| n.as_str().starts_with("org.mpris.MediaPlayer2."))
        .collect();

    for name in &mpris_names {
        let service_pid = dbus
            .get_connection_unix_process_id(name.as_ref())
            .await
            .ok();

        if service_pid != Some(pid) {
            continue;
        }

        let props = zbus::fdo::PropertiesProxy::builder(conn)
            .destination(name.as_str())
            .ok()?
            .path("/org/mpris/MediaPlayer2")
            .ok()?
            .build()
            .await
            .ok()?;

        let metadata: OwnedValue = props
            .get("org.mpris.MediaPlayer2.Player".try_into().ok()?, "Metadata")
            .await
            .ok()?;

        let Value::Dict(dict) = &*metadata else {
            continue;
        };

        let art_key = Value::from("mpris:artUrl");
        let art_value = dict.get::<_, Value<'_>>(&art_key).ok().flatten();
        let url = match &art_value {
            Some(Value::Str(s)) => Some(s.as_str()),
            _ => None,
        };
        if let Some(data) = url
            .and_then(|u| u.strip_prefix("file://"))
            .and_then(|path| std::fs::read(path).ok())
        {
            return Some(data);
        }
    }

    None
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
/// Only queries MPRIS when a single audio stream exists for a given PID.
/// Uses a persistent cache so data survives channel destruction/recreation.
pub async fn enrich_with_mpris(applications: &mut Vec<AppInfo>) {
    // Count how many streams share each PID
    let mut pid_counts: HashMap<u32, usize> = HashMap::new();
    for app in applications.iter() {
        if let Some(pid) = app.pid {
            *pid_counts.entry(pid).or_default() += 1;
        }
    }

    let mut cache = CACHE.lock().await;

    for app in applications.iter_mut() {
        let Some(pid) = app.pid else { continue };

        // Only query MPRIS when a single stream uses this PID
        if pid_counts.get(&pid).copied().unwrap_or(0) == 1 {
            if let Some(art_data) = get_mpris_art(pid).await {
                // Fresh MPRIS art — update cache and app
                cache.insert(pid, PidCache {
                    art_data: art_data.clone(),
                    display_name: app.app_name.clone(),
                });
                app.mpris_art_data = Some(art_data);
            } else if let Some(cached) = cache.get(&pid) {
                // MPRIS query failed — use cached art
                app.mpris_art_data = Some(cached.art_data.clone());
            }
        }

        // If the app name is generic, restore the last known good name from cache
        if is_generic_name(&app.app_name) {
            if let Some(cached) = cache.get(&pid) {
                if !is_generic_name(&cached.display_name) {
                    app.app_name = cached.display_name.clone();
                }
            }
        } else {
            // Update the cached display name if we have a good one
            if let Some(cached) = cache.get_mut(&pid) {
                cached.display_name = app.app_name.clone();
            }
        }
    }

    // Only remove cache entries for PIDs whose process no longer exists
    cache.retain(|&pid, _| {
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    });
}

/// Monitor MPRIS PropertiesChanged signals on D-Bus.
/// When metadata changes, trigger a refresh with a short delay to let metadata stabilize.
pub fn start_mpris_monitoring() {
    tokio::spawn(async {
        if let Err(e) = mpris_monitor_loop().await {
            eprintln!("MPRIS monitor error: {e}");
        }
    });
}

async fn mpris_monitor_loop() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::session().await?;

    let rule = "type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',arg0='org.mpris.MediaPlayer2.Player'";
    let proxy = zbus::fdo::DBusProxy::new(&conn).await?;
    proxy.add_match_rule(rule.try_into()?).await?;

    let rule2 = "type='signal',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0namespace='org.mpris.MediaPlayer2'";
    proxy.add_match_rule(rule2.try_into()?).await?;

    use futures_lite::StreamExt;
    let mut stream = zbus::MessageStream::from(&conn);

    while let Some(msg) = stream.next().await {
        let Ok(msg): Result<zbus::Message, _> = msg else {
            continue;
        };
        let member = msg.header().member().map(|m| m.to_string());

        if member.as_deref() == Some("PropertiesChanged")
            || member.as_deref() == Some("NameOwnerChanged")
        {
            // Delay to let MPRIS metadata fully update before we re-query
            tokio::time::sleep(Duration::from_millis(500)).await;
            crate::audio::pulse::pulse_monitor::request_refresh();
        }
    }

    Ok(())
}
