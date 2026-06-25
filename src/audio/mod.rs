pub mod audio_system;
pub mod pulse;

pub use audio_system::{AppInfo, AudioSystem};
pub use pulse::PulseAudioSystem;

/// Create a fresh audio-system handle. Returns `Err` (rather than panicking)
/// when PulseAudio is momentarily unreachable — e.g. mid-restart — so the
/// caller can skip the operation and recover on the next event instead of
/// taking down the handler task.
pub fn create() -> Result<Box<dyn AudioSystem>, Box<dyn std::error::Error>> {
    Ok(Box::new(PulseAudioSystem::new()?))
}
