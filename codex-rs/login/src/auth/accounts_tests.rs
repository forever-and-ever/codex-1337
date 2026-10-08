use super::*;
use crate::auth::AuthManager;
use crate::auth::CodexAuth;
use crate::auth::storage::FileAuthStorage;
use base64::Engine;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use serde_json::json;

fn account(id: &str, revision: usize) -> AuthDotJson {
    let claims = json!({
        "email": format!("{id}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_user_id": id,
            "chatgpt_account_id": id,
            "chatgpt_plan_type": "pro"
        }
    });
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&claims).unwrap());
    serde_json::from_value(json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": format!("e30.{claims}.c2ln"),
            "access_token": format!("access-{id}-{revision}"),
            "refresh_token": format!("refresh-{id}-{revision}"),
            "account_id": id
        }
    }))
    .unwrap()
}

// Login, refresh, deletion, and another process's stale refresh share one document.
// A writer must preserve unrelated accounts and lose to a newer login or deletion.
#[test]
fn account_transactions_preserve_other_logins_and_reject_stale_writers() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let backend: Arc<dyn AuthStorageBackend> =
        Arc::new(FileAuthStorage::new(home.path().to_owned()));
    let mut alice = account("alice", /*revision*/ 0);
    // Browser login can include an exchanged API key alongside explicit ChatGPT auth.
    alice.openai_api_key = Some("exchanged-key".into());
    let bob = account("bob", /*revision*/ 0);
    backend.save(&alice)?;
    backend.save(&bob)?;
    let scoped = AccountStorage {
        backend: backend.clone(),
        id: identity(&alice).unwrap(),
    };
    assert_eq!(scoped.load()?, Some(alice.clone()));
    assert_eq!(backend.load()?, Some(bob.clone()));

    let refreshed = account("alice", /*revision*/ 1);
    assert!(scoped.compare_exchange(&alice, &refreshed)?);
    assert_eq!(backend.load()?, Some(bob));
    let relogged = account("alice", /*revision*/ 2);
    backend.save(&relogged)?;
    assert!(!scoped.compare_exchange(&refreshed, &account("alice", /*revision*/ 3))?);
    assert_eq!(scoped.load()?, Some(relogged.clone()));
    assert!(scoped.delete()?);
    assert!(!scoped.compare_exchange(&relogged, &account("alice", /*revision*/ 4))?);
    assert!(scoped.save(&relogged).is_err());
    assert_eq!(scoped.load()?, None);
    assert_eq!(backend.load()?, Some(account("bob", /*revision*/ 0)));
    Ok(())
}

#[tokio::test]
async fn conversation_selectors_do_not_follow_global_login_or_each_other() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let backend = FileAuthStorage::new(home.path().to_owned());
    backend.save(&account("alice", /*revision*/ 0))?;
    backend.save(&account("bob", /*revision*/ 0))?;
    let root = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("test"),
        home.path().to_owned(),
    );
    let alice_id = identity(&account("alice", /*revision*/ 0)).unwrap();
    let bob_id = identity(&account("bob", /*revision*/ 0)).unwrap();
    let first = root.for_account(&alice_id).await?;
    let second = first.for_account(&alice_id).await?;
    first.select_account(&bob_id).await?;
    backend.save(&account("alice", /*revision*/ 1))?;
    first.reload().await;
    second.reload().await;
    assert_eq!(
        first.auth_cached().unwrap().get_account_id().as_deref(),
        Some("bob")
    );
    assert_eq!(
        second.auth_cached().unwrap().get_account_id().as_deref(),
        Some("alice")
    );
    assert_eq!(first.auth_cached().unwrap().auth_mode(), AuthMode::Chatgpt);
    assert!(first.select_account("missing").await.is_err());
    assert_eq!(first.selected_account_id(), Some(bob_id));
    // A failed read used to erase the scoped cache as if logout had occurred.
    // Neither reader may switch to the global login during a storage outage.
    let auth_file = home.path().join("auth.json");
    let preserved = home.path().join("auth.saved");
    std::fs::rename(&auth_file, &preserved)?;
    std::fs::create_dir(&auth_file)?;
    assert_eq!(
        first.auth().await.unwrap().get_account_id().as_deref(),
        Some("bob")
    );
    assert_eq!(
        second.auth().await.unwrap().get_account_id().as_deref(),
        Some("alice")
    );
    std::fs::remove_dir(&auth_file)?;
    std::fs::rename(&preserved, &auth_file)?;
    second.logout().await?;
    assert!(second.auth().await.is_none());
    let missing = root.for_account(&alice_id).await?;
    missing.reload().await;
    assert_eq!(missing.selected_account_id(), Some(alice_id));
    assert!(missing.auth_cached().is_none());
    assert!(!missing.logout_with_revoke().await?);
    assert_eq!(backend.load()?, Some(account("bob", /*revision*/ 0)));
    backend.save(&account("alice", /*revision*/ 2))?;
    assert_eq!(
        missing.auth().await.unwrap().get_account_id().as_deref(),
        Some("alice")
    );
    assert_eq!(
        first.auth_cached().unwrap().get_account_id().as_deref(),
        Some("bob")
    );
    Ok(())
}
