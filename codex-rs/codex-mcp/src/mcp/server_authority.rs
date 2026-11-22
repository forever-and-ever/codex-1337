//! One rule assigns an MCP server its permission authority, whether for a
//! published runtime or for the turn step that issues a call.

use codex_protocol::models::PermissionProfile;

use super::McpConfig;
use crate::McpServerKey;
use crate::McpServerSource;

impl McpConfig {
    /// Selects the permissions governing `server_name` from its owner's thread
    /// permissions and the permissions of the environment hosting it.
    /// `None` means that owner holds no authority over the server.
    pub fn server_permission_profile<'a>(
        &self,
        server_key: &McpServerKey,
        thread_profile: &'a PermissionProfile,
        environment_profile: impl FnOnce(&str) -> Option<&'a PermissionProfile>,
    ) -> Option<&'a PermissionProfile> {
        self.server_authority(server_key, || thread_profile, environment_profile)
    }

    pub fn server_permission_source(
        &self,
        server_key: &McpServerKey,
        environment: impl FnOnce(
            &str,
        )
            -> Option<std::sync::Arc<codex_exec_server::FileSystemSandboxContext>>,
    ) -> Option<super::McpServerPermissions> {
        self.server_authority(
            server_key,
            || super::McpServerPermissions::Thread,
            |id| environment(id).map(super::McpServerPermissions::Environment),
        )
    }

    fn server_authority<T>(
        &self,
        server_key: &McpServerKey,
        thread: impl FnOnce() -> T,
        environment: impl FnOnce(&str) -> Option<T>,
    ) -> Option<T> {
        let server = self.mcp_server_catalog.server(server_key)?;
        if server
            .source()
            .is_host_owned_apps(server_key.name(), server.config())
        {
            return Some(thread());
        }
        environment(&server.config().environment_id).or_else(|| {
            (server.config().is_local_environment()
                || matches!(server.source(), McpServerSource::SelectedPlugin(_)))
            .then(thread)
        })
    }
}
