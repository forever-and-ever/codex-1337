use super::AuthManager;
use crate::AuthRouteConfig;
use crate::CodexAuth;
use crate::ExternalAuth;
use crate::ExternalAuthFuture;
use crate::ExternalAuthRefreshContext;
use codex_http_client::DestinationPolicy;
use codex_http_client::NetworkPolicyController;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::Notify;
use tokio::sync::oneshot;

use base64::Engine;
use serde_json::json;

struct HeldSource {
    auth: CodexAuth,
    pending: Mutex<Option<oneshot::Receiver<CodexAuth>>>,
    entered: Notify,
}

impl HeldSource {
    fn new(auth: CodexAuth) -> Self {
        Self {
            auth,
            pending: Mutex::new(None),
            entered: Notify::new(),
        }
    }
}

impl ExternalAuth for HeldSource {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        let pending = self.pending.lock().unwrap().take();
        Box::pin(async move {
            if let Some(pending) = pending {
                self.entered.notify_one();
                return pending.await.map_err(std::io::Error::other);
            }
            Ok(self.auth.clone())
        })
    }

    fn refresh(&self, _: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        self.resolve()
    }
}

impl AuthManager {
    pub(in crate::auth) async fn exercise_source_replacement_fence() {
        let auth = |workspace: &str, token: &str| {
            let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                json!({"jti": token, "https://api.openai.com/auth": {
                    "chatgpt_user_id": "user-a",
                }})
                .to_string(),
            );
            CodexAuth::from_external_chatgpt_tokens(
                &format!("header.{payload}.signature"),
                workspace,
                None,
            )
            .unwrap()
        };
        let initial = auth("workspace-a", "current-token");
        let stale = auth("workspace-a", "retired-token");
        let other = auth("retired-workspace", "retired-token");
        for retired in [stale, other] {
            let home = tempfile::tempdir().unwrap();
            let mut manager =
                Self::from_auth_for_testing_with_home(initial.clone(), home.path().to_owned());
            let controller = NetworkPolicyController::default();
            let policy = controller.policy();
            Arc::get_mut(&mut manager).unwrap().auth_route_config =
                AuthRouteConfig::from_http_client_factory(
                    manager
                        .http_client_factory()
                        .with_network_policy(policy.clone()),
                );
            let source = Arc::new(HeldSource::new(initial.clone()));
            manager.set_external_auth(source.clone()).await.unwrap();
            assert!(controller.publish(policy.revision(), DestinationPolicy::Unrestricted));
            let permit = policy
                .acquire(&"https://example.com/".parse().unwrap())
                .unwrap();

            // B has identical credentials to A. Snapshot comparison cannot fence A's
            // pending read; source freshness must advance independently of ownership.
            let (release, pending) = oneshot::channel();
            *source.pending.lock().unwrap() = Some(pending);
            let reloading = Arc::clone(&manager);
            let reload = tokio::spawn(async move { reloading.reload().await });
            source.entered.notified().await;
            let before = *manager.auth_change_state_receiver().borrow();
            let mut notifications = manager.auth_change_receiver();
            let revision = *notifications.borrow_and_update();
            manager
                .set_external_auth(Arc::new(HeldSource::new(initial.clone())))
                .await
                .unwrap();
            let installed = *manager.auth_change_state_receiver().borrow();
            assert_eq!(manager.auth_cached(), Some(initial.clone()));
            permit.check().unwrap();
            release.send(retired).unwrap();
            reload.await.unwrap();
            assert_eq!(
                manager.auth_cached().unwrap().get_token().unwrap(),
                initial.get_token().unwrap(),
                "retired source must not publish after its replacement is installed"
            );
            assert_eq!(installed.generation, before.generation + 1);
            assert_eq!(installed.owner_generation, before.owner_generation);
            assert_eq!(installed.source_generation, before.source_generation);
            assert_eq!(installed.login_generation, before.login_generation);
            assert_eq!(
                *manager.auth_change_state_receiver().borrow(),
                installed,
                "no transient retired-source publication may hide behind a later repair"
            );
            assert_eq!(*notifications.borrow_and_update(), revision + 1);
            permit.check().unwrap();
        }
    }
}
