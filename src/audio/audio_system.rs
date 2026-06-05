use std::error::Error;

#[derive(Debug)]
pub struct AppInfo {
    pub uid: u32,
    pub app_name: String,
    pub icon_search_name: String,
    pub pid: Option<u32>,
    pub sink_name: Option<String>,
    pub mute: bool,
    pub vol_percent: f32,
    pub icon_name: Option<String>,
    pub is_device: bool,
    pub is_multi_sink_app: bool,
    /// PID from the owning PulseAudio *client* proplist. For some apps (e.g.
    /// pipewire-native ones) the sink-input has no PID but the client does.
    pub client_pid: Option<u32>,
    /// `application.name` from the client proplist.
    pub client_name: Option<String>,
    /// `application.process.binary` from the client proplist — a clean icon key.
    pub client_binary: Option<String>,
    /// Normalized app-id of the matched XWayland window (lowercased, `.exe`
    /// stripped), used as a high-quality themed-icon lookup key.
    pub wm_class: Option<String>,
    /// Real icon pixels pulled from the matched window's `_NET_WM_ICON`.
    pub window_icon: Option<crate::window_icons::WindowIcon>,
}

pub trait AudioSystem {
    fn list_applications(&mut self) -> Result<Vec<AppInfo>, Box<dyn Error>>;
    fn increase_volume(
        &mut self,
        app_index: u32,
        percent: f64,
        is_device: bool,
    ) -> Result<(), Box<dyn Error>>;
    fn decrease_volume(
        &mut self,
        app_index: u32,
        percent: f64,
        is_device: bool,
    ) -> Result<(), Box<dyn Error>>;
    fn mute_volume(
        &mut self,
        app_index: u32,
        mute: bool,
        is_device: bool,
    ) -> Result<(), Box<dyn Error>>;
}
