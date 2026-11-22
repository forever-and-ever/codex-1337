//! Covers portable network configuration projection.

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn attachment_projection_preserves_policy_and_drops_listener_addresses() {
    for raw in [
        "",
        r#"
enabled = false
allow_upstream_proxy = false
dangerously_allow_all_unix_sockets = false
allow_local_binding = false
[domains]
'allowed.example' = 'allow'
'blocked.example' = 'deny'
[unix_sockets]
'/tmp/allowed.sock' = 'allow'
'/tmp/blocked.sock' = 'deny'
'relative.sock' = 'deny'
'~/blocked.sock' = 'deny'
'C:\blocked.sock' = 'deny'
"#,
        r#"[unix_sockets]
'C:\allowed.sock' = 'allow'
'\\server\share\allowed.sock' = 'allow'
"#,
    ] {
        let expected: NetworkToml = toml::from_str(raw).unwrap();
        let network = NetworkToml {
            proxy_url: Some("not a listener URL".to_string()),
            socks_url: Some("socks5://127.0.0.1:1080".to_string()),
            ..expected.clone()
        };
        assert_eq!(
            project_environment_profile_network(Some(network)),
            Ok(Some(expected)),
            "{raw}"
        );
    }
    assert_eq!(
        project_environment_profile_network(/*network*/ None),
        Ok(None)
    );
}

#[test]
fn attachment_projection_rejects_unsupported_restrictions_and_defers_policy_validation() {
    for raw in [
        "mode = 'full'",
        "mode = 'limited'",
        r#"
[mitm.actions.redact]
strip_request_headers = ['Authorization']
[mitm.hooks.api]
host = 'api.example'
methods = ['GET']
path_prefixes = ['/']
action = ['redact']
"#,
    ] {
        let network: NetworkToml = toml::from_str(raw).unwrap();
        assert_eq!(
            project_environment_profile_network(Some(network)),
            Err(EnvironmentNetworkConfigError),
            "{raw}"
        );
    }

    for raw in [
        "[unix_sockets]\n'relative.sock' = 'allow'",
        "[unix_sockets]\n'~/allowed.sock' = 'allow'",
        "[unix_sockets]\n\"/tmp/\\u0000allowed.sock\" = 'allow'",
        "[domains]\n'api[12].example' = 'allow'",
    ] {
        let network: NetworkToml = toml::from_str(raw).unwrap();
        let projected = project_environment_profile_network(Some(network.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(projected, network);
        let policy = EnvironmentNetworkPolicy::from_config(
            &projected.to_network_proxy_config(),
            /*managed_allowed_domains_only*/ false,
        );
        assert_eq!(
            validate_environment_network_policy(
                &policy,
                &PermissionProfile::read_only(),
                Platform::Unknown,
            ),
            Err(EnvironmentNetworkConfigError),
            "{raw}"
        );
    }
}
