// Aggregates all former standalone integration tests as modules.
mod bundled_bwrap;
mod bwrap_path;
#[cfg(target_os = "linux")]
mod managed_proxy_bridge_lifecycle_tests;
mod sandbox;
