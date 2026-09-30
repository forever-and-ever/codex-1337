use pretty_assertions::assert_eq;

use super::RemoteNetworkProxyConfig;
use super::RemoteNetworkProxyLaunchConfig;
use crate::MitmHookConfig;
use crate::NetworkMitmCaConfig;
use crate::NetworkMode;
use crate::NetworkProxyConfig;

#[test]
fn optional_socket_policy_preserves_input_and_resolves_at_remote_boundary() {
    for allow_all in [None, Some(false), Some(true)] {
        let mut config = NetworkProxyConfig {
            dangerously_allow_all_unix_sockets: allow_all,
            ..NetworkProxyConfig::default()
        };
        config.set_allow_unix_sockets(Vec::new());
        let serialized = serde_json::to_value(&config).expect("serialize config");
        assert_eq!(
            serialized.get("dangerously_allow_all_unix_sockets"),
            allow_all.map(serde_json::Value::Bool).as_ref()
        );
        assert_eq!(serialized["unix_sockets"], serde_json::json!({}));
        assert_eq!(
            serde_json::from_value::<NetworkProxyConfig>(serialized).expect("deserialize config"),
            config
        );

        let remote = RemoteNetworkProxyConfig::from_effective_config(&config)
            .expect("supported remote config");
        assert_eq!(
            remote.dangerously_allow_all_unix_sockets,
            allow_all.unwrap_or(false)
        );
    }
}

#[test]
fn round_trip_preserves_supported_effective_settings() {
    for socket in ["/var/run/example.sock", r"C:\example.sock"] {
        let mut config = NetworkProxyConfig {
            enabled: true,
            enable_socks5: false,
            enable_socks5_udp: false,
            allow_upstream_proxy: false,
            dangerously_allow_all_unix_sockets: Some(true),
            mode: NetworkMode::Limited,
            allow_local_binding: Some(true),
            ..NetworkProxyConfig::default()
        };
        config.set_allowed_domains(vec!["example.com".into()]);
        config.set_denied_domains(vec!["blocked.example.com".into()]);
        config.set_allow_unix_sockets(vec![socket.into()]);

        let remote = RemoteNetworkProxyConfig::from_effective_config(&config)
            .expect("supported remote config");
        let wire = serde_json::to_value(&remote).expect("serialize remote config");
        let round_trip = serde_json::from_value::<RemoteNetworkProxyConfig>(wire.clone())
            .expect("deserialize remote config");

        assert_eq!(
            serde_json::to_value(round_trip).expect("serialize round-tripped remote config"),
            wire,
        );
    }
}

#[test]
fn rejects_unsupported_configuration() {
    let cases = [
        (
            "MITM",
            NetworkProxyConfig {
                enabled: true,
                mitm: true,
                ..NetworkProxyConfig::default()
            },
        ),
        (
            "credential broker",
            NetworkProxyConfig {
                enabled: true,
                credential_broker: true,
                ..NetworkProxyConfig::default()
            },
        ),
        (
            "plaintext credential injection",
            NetworkProxyConfig {
                enabled: true,
                dangerously_allow_plaintext_credential_injection: true,
                ..NetworkProxyConfig::default()
            },
        ),
        (
            "MITM hooks",
            NetworkProxyConfig {
                enabled: true,
                mitm_hooks: vec![MitmHookConfig::default()],
                ..NetworkProxyConfig::default()
            },
        ),
    ];

    for (feature, config) in cases {
        assert!(
            RemoteNetworkProxyConfig::from_effective_config(&config).is_err(),
            "{feature} must not cross the remote executor boundary"
        );
    }
}

#[test]
fn accepts_unsupported_configuration_when_proxy_is_disabled() {
    let config = NetworkProxyConfig {
        mitm: true,
        credential_broker: true,
        dangerously_allow_plaintext_credential_injection: true,
        mitm_hooks: vec![MitmHookConfig::default()],
        ..NetworkProxyConfig::default()
    };

    let remote = RemoteNetworkProxyConfig::from_effective_config(&config)
        .expect("disabled proxy configuration does not cross the executor boundary");

    assert!(!remote.enabled);
}

#[test]
fn rejects_external_ca_even_when_proxy_is_disabled() {
    for (enabled, mitm_enabled) in [(false, false), (false, true), (true, false), (true, true)] {
        let config = NetworkProxyConfig {
            enabled,
            mitm: mitm_enabled,
            mitm_ca: Some(NetworkMitmCaConfig {
                certificate_file: "/run/proxy/ca.pem".to_string(),
                private_key_file: "/run/proxy/key.pem".to_string(),
            }),
            ..NetworkProxyConfig::default()
        };

        assert!(
            RemoteNetworkProxyConfig::from_effective_config(&config).is_err(),
            "external CA configuration must not cross the remote executor boundary (proxy enabled={enabled}, MITM enabled={mitm_enabled})"
        );
    }
}

#[test]
fn policy_decision_callback_timeout_round_trips() {
    let config = RemoteNetworkProxyConfig::from_effective_config(&NetworkProxyConfig::default())
        .expect("supported remote config");
    let mut launch = RemoteNetworkProxyLaunchConfig::new(config);
    let without_timeout = serde_json::to_value(&launch).expect("serialize launch config");
    assert_eq!(without_timeout.get("policyDecisionTimeoutMs"), None);
    launch.policy_decision_timeout_ms = Some(900_000);
    let with_timeout = serde_json::to_value(&launch).expect("serialize launch timeout");
    assert_eq!(with_timeout["policyDecisionTimeoutMs"], 900_000);
    assert_eq!(
        serde_json::from_value::<RemoteNetworkProxyLaunchConfig>(with_timeout)
            .expect("deserialize launch timeout"),
        launch
    );
}
