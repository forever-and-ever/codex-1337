// Aggregates all former standalone integration tests as modules.
#[cfg(unix)]
mod daemon_compatibility;
#[cfg(unix)]
mod directory_trust;
#[cfg(unix)]
mod focus_palette;
#[cfg(unix)]
mod provider_defaults;
#[cfg(unix)]
mod reconnect;
mod resize_reflow;
#[cfg(unix)]
mod screen_reader;
mod status_indicator;
#[cfg(unix)]
mod thread_title;
#[cfg(target_os = "linux")]
mod ui_reload;
#[cfg(unix)]
mod unix_daemon;
mod vt100_history;
mod vt100_live_commit;
