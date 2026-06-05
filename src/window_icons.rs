//! Best-effort recovery of application icons from XWayland windows.
//!
//! On a Wayland session, X11 apps (games, launchers, Electron/CEF apps) run
//! under XWayland and still publish the classic X11 window properties. We use
//! those to attach a real icon to non-browser audio streams that the icon-theme
//! lookup alone can't resolve:
//!   - `WM_CLASS` gives a clean app-id (e.g. "starcitizen") that maps onto the
//!     installed themed icon far better than the messy PulseAudio stream name.
//!   - `_NET_WM_ICON` carries the actual icon pixels — the only option for
//!     themeless apps (Wine games), and sometimes higher quality than the theme.
//!
//! Everything here is read-only and degrades gracefully: no DISPLAY / no
//! XWayland / native-Wayland apps simply yield no match and the caller falls
//! back to the existing theme/default behavior.

use crate::audio::audio_system::AppInfo;
use std::fmt;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt, Window};
use x11rb::rust_connection::RustConnection;

/// Decoded icon pixels pulled from a window's `_NET_WM_ICON`, PNG-encoded.
#[derive(Clone)]
pub struct WindowIcon {
    pub max_dim: u32,
    pub png: Vec<u8>,
}

impl fmt::Debug for WindowIcon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowIcon")
            .field("max_dim", &self.max_dim)
            .field("png_bytes", &self.png.len())
            .finish()
    }
}

/// Normalize an app identifier for matching/lookup: lowercase, trim, drop a
/// trailing `.exe` (Wine windows report e.g. "starcitizen.exe").
fn normalize(s: &str) -> String {
    let lower = s.trim().to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).trim().to_string()
}

/// Reduce a window title to a clean app name by dropping trailing runtime
/// decorations: everything from the first " (" or " - " onward.
/// "Baldur's Gate 3 (3840x2160) - (Vulkan)" -> "Baldur's Gate 3".
fn clean_title(raw: &str) -> String {
    let t = raw.trim();
    match [" (", " - "].iter().filter_map(|d| t.find(d)).min() {
        Some(i) => t[..i].trim().to_string(),
        None => t.to_string(),
    }
}

struct WindowRef {
    window: Window,
    pid: Option<u32>,
    /// Normalized `WM_CLASS` strings (instance + class), deduped.
    classes: Vec<String>,
    /// Normalized `_NET_WM_NAME` (for matching).
    name: Option<String>,
    /// Raw `_NET_WM_NAME` (for display).
    title: Option<String>,
}

/// Choose the window that best corresponds to an audio stream. Pure so it can be
/// unit-tested. Priority: exact PID, then WM_CLASS match, then window-name match.
fn pick_window<'a>(
    windows: &'a [WindowRef],
    pids: &[Option<u32>],
    names: &[String],
) -> Option<&'a WindowRef> {
    // 1. PID — reliable for single-process apps (native games, Spotify).
    if let Some(w) = windows
        .iter()
        .find(|w| w.pid.is_some() && pids.iter().any(|p| p.is_some() && *p == w.pid))
    {
        return Some(w);
    }
    // 2. WM_CLASS — bridges CEF/Electron apps whose audio runs in a detached
    //    utility process (PID won't match the window).
    if let Some(w) = windows
        .iter()
        .find(|w| names.iter().any(|n| w.classes.iter().any(|c| c == n)))
    {
        return Some(w);
    }
    // 3. Window title — last resort.
    windows
        .iter()
        .find(|w| w.name.as_deref().is_some_and(|nm| names.iter().any(|n| n == nm)))
}

struct Atoms {
    net_client_list: Atom,
    net_wm_pid: Atom,
    net_wm_icon: Atom,
    net_wm_name: Atom,
}

fn intern(conn: &RustConnection, name: &[u8]) -> Option<Atom> {
    conn.intern_atom(false, name).ok()?.reply().ok().map(|r| r.atom)
}

/// Read a 32-bit-format property as a `Vec<u32>` (CARDINAL/WINDOW lists).
fn prop_u32s(conn: &RustConnection, win: Window, prop: Atom) -> Vec<u32> {
    conn.get_property(false, win, prop, AtomEnum::ANY, 0, u32::MAX)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| r.value32().map(|it| it.collect()))
        .unwrap_or_default()
}

/// Read an 8-bit-format property as raw bytes (STRING/UTF8_STRING).
fn prop_bytes(conn: &RustConnection, win: Window, prop: Atom) -> Option<Vec<u8>> {
    let r = conn
        .get_property(false, win, prop, AtomEnum::ANY, 0, u32::MAX)
        .ok()?
        .reply()
        .ok()?;
    if r.value.is_empty() { None } else { Some(r.value) }
}

/// `_NET_WM_ICON` is one or more images concatenated, each laid out as
/// `[width, height, width*height pixels...]` where every pixel is a 32-bit
/// non-premultiplied `0xAARRGGBB`. Return the largest image as (w, h, RGBA).
fn parse_largest_icon(data: &[u32]) -> Option<(u32, u32, Vec<u8>)> {
    let mut best: Option<(usize, usize, usize)> = None; // (w, h, start index of pixels)
    let mut i = 0usize;
    while i + 2 <= data.len() {
        let w = data[i] as usize;
        let h = data[i + 1] as usize;
        i += 2;
        if w == 0 || h == 0 {
            break;
        }
        let count = match w.checked_mul(h) {
            Some(c) => c,
            None => break,
        };
        if i + count > data.len() {
            break;
        }
        if best.is_none_or(|(bw, bh, _)| w * h > bw * bh) {
            best = Some((w, h, i));
        }
        i += count;
    }

    let (w, h, start) = best?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for &px in &data[start..start + w * h] {
        rgba.push(((px >> 16) & 0xff) as u8); // R
        rgba.push(((px >> 8) & 0xff) as u8); // G
        rgba.push((px & 0xff) as u8); // B
        rgba.push(((px >> 24) & 0xff) as u8); // A
    }
    Some((w as u32, h as u32, rgba))
}

fn icon_to_png(data: &[u32]) -> Option<WindowIcon> {
    let (w, h, rgba) = parse_largest_icon(data)?;
    let img = image::RgbaImage::from_raw(w, h, rgba)?;
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .ok()?;
    Some(WindowIcon { max_dim: w.max(h), png: buf })
}

struct Snapshot {
    conn: RustConnection,
    atoms: Atoms,
    windows: Vec<WindowRef>,
}

impl Snapshot {
    /// Connect to the X server (XWayland) and read every toplevel's identity.
    /// `_NET_WM_ICON` is intentionally *not* read here — only for matched windows.
    fn capture() -> Option<Self> {
        let (conn, screen_num) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen_num)?.root;
        let atoms = Atoms {
            net_client_list: intern(&conn, b"_NET_CLIENT_LIST")?,
            net_wm_pid: intern(&conn, b"_NET_WM_PID")?,
            net_wm_icon: intern(&conn, b"_NET_WM_ICON")?,
            net_wm_name: intern(&conn, b"_NET_WM_NAME")?,
        };

        let mut windows = Vec::new();
        for id in prop_u32s(&conn, root, atoms.net_client_list) {
            let pid = prop_u32s(&conn, id, atoms.net_wm_pid).first().copied();

            let mut classes: Vec<String> = Vec::new();
            if let Some(bytes) = prop_bytes(&conn, id, AtomEnum::WM_CLASS.into()) {
                for part in bytes.split(|&b| b == 0) {
                    if part.is_empty() {
                        continue;
                    }
                    let key = normalize(&String::from_utf8_lossy(part));
                    if !key.is_empty() && !classes.contains(&key) {
                        classes.push(key);
                    }
                }
            }

            let title = prop_bytes(&conn, id, atoms.net_wm_name)
                .map(|b| String::from_utf8_lossy(&b).trim().to_string())
                .filter(|s| !s.is_empty());
            let name = title.as_deref().map(normalize).filter(|s| !s.is_empty());

            windows.push(WindowRef { window: id, pid, classes, name, title });
        }

        Some(Self { conn, atoms, windows })
    }

    fn read_icon(&self, win: Window) -> Option<WindowIcon> {
        let data = prop_u32s(&self.conn, win, self.atoms.net_wm_icon);
        if data.is_empty() {
            return None;
        }
        icon_to_png(&data)
    }
}

/// Attach `wm_class` and `window_icon` to each non-browser stream by matching it
/// to an XWayland window. No-op when there's no reachable X server.
pub fn enrich(apps: &mut [AppInfo]) {
    let Some(snap) = Snapshot::capture() else {
        return;
    };
    if snap.windows.is_empty() {
        return;
    }

    for app in apps.iter_mut() {
        if app.is_device {
            continue;
        }

        let pids = [app.pid, app.client_pid];

        let mut names: Vec<String> = Vec::new();
        for candidate in [
            Some(app.app_name.as_str()),
            app.client_name.as_deref(),
            app.client_binary.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let key = normalize(candidate);
            if !key.is_empty() && !names.contains(&key) {
                names.push(key);
            }
        }

        if let Some(w) = pick_window(&snap.windows, &pids, &names) {
            app.wm_class = w.classes.first().cloned();
            app.window_icon = snap.read_icon(w.window);

            // Always prefer a confident window-derived name over the audio stream
            // name, which is often an engine/framework name (e.g. "Wwise" for
            // Baldur's Gate 3, "Chromium" for Steam). See the project memory
            // "window-name-override-policy" before changing this.
            if let Some(raw) = w.title.as_deref() {
                let name = clean_title(raw);
                if !name.is_empty() {
                    app.app_name = name;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(pid: Option<u32>, classes: &[&str], name: Option<&str>) -> WindowRef {
        WindowRef {
            window: 0,
            pid,
            classes: classes.iter().map(|c| normalize(c)).collect(),
            name: name.map(normalize),
            title: name.map(|s| s.to_string()),
        }
    }

    #[test]
    fn normalize_strips_exe_and_lowercases() {
        assert_eq!(normalize("StarCitizen.exe"), "starcitizen");
        assert_eq!(normalize("  Star Citizen "), "star citizen");
    }

    #[test]
    fn clean_title_strips_runtime_decorations() {
        assert_eq!(
            clean_title("Baldur's Gate 3 (3840x2160) - (Vulkan) - (6 + 6 WT)"),
            "Baldur's Gate 3"
        );
        assert_eq!(clean_title("Star Citizen "), "Star Citizen");
        assert_eq!(clean_title("Steam"), "Steam");
    }

    #[test]
    fn pid_match_wins_over_name() {
        let windows = vec![
            win(Some(2393666), &["starcitizen.exe"], Some("Star Citizen")),
            win(Some(999), &["other"], Some("other")),
        ];
        let pick = pick_window(&windows, &[Some(2393666), None], &["other".into()]);
        assert_eq!(pick.unwrap().pid, Some(2393666));
    }

    #[test]
    fn class_match_bridges_cef_pid_mismatch() {
        // Audio PID (utility process) doesn't match the window PID; class does.
        let windows = vec![win(Some(2565940), &["steamwebhelper", "steam"], Some("Steam"))];
        let pick = pick_window(&windows, &[Some(2569877), None], &["steamwebhelper".into()]);
        assert!(pick.is_some());
    }

    #[test]
    fn no_match_returns_none() {
        let windows = vec![win(Some(1), &["foo"], Some("foo"))];
        assert!(pick_window(&windows, &[Some(2), None], &["bar".into()]).is_none());
    }

    #[test]
    fn parses_largest_of_two_icons() {
        // Two images: 1x1 then 2x2. Pixels are 0xAARRGGBB.
        let data: Vec<u32> = vec![
            1, 1, 0xFF112233, // 1x1
            2, 2, 0xFFAABBCC, 0xFF445566, 0xFF778899, 0xFF000000, // 2x2
        ];
        let (w, h, rgba) = parse_largest_icon(&data).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(rgba.len(), 2 * 2 * 4);
        // First pixel 0xFFAABBCC -> R=AA, G=BB, B=CC, A=FF
        assert_eq!(&rgba[0..4], &[0xAA, 0xBB, 0xCC, 0xFF]);
    }

    #[test]
    fn rejects_truncated_icon_data() {
        // Claims 4x4 but doesn't provide the pixels.
        let data: Vec<u32> = vec![4, 4, 0xFFFFFFFF];
        assert!(parse_largest_icon(&data).is_none());
    }
}
