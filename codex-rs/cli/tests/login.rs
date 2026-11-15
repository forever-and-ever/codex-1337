use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::write_chatgpt_auth;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::CODEX_ACCESS_TOKEN_ENV_VAR;
use codex_login::REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR;
use codex_protocol::shell_environment::OPENAI_FEDERATION_RULE_ID_ENV_VAR;
use codex_protocol::shell_environment::OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home);
    Ok(cmd)
}

fn write_file_auth_config(codex_home: &Path) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        "cli_auth_credentials_store = \"file\"\n",
    )?;
    Ok(())
}

fn read_auth_json(codex_home: &Path) -> Result<Value> {
    let auth_json = std::fs::read_to_string(codex_home.join("auth.json"))?;
    Ok(serde_json::from_str(&auth_json)?)
}

const SITES_GIT_REQUEST: &str = "capability[]=authtype\nprotocol=https\nhost=chatgpt.com\npath=backend-api/codex/sites/git/fixture.git\n\n";

#[test]
fn git_credential_advertises_authtype_before_login() -> Result<()> {
    let codex_home = TempDir::new()?;
    codex_command(codex_home.path())?
        .args(["git-credential", "capability"])
        .assert()
        .success()
        .stdout(contains("capability authtype\n"));
    Ok(())
}

#[test]
fn git_credential_reads_only_the_first_record() -> Result<()> {
    let home = TempDir::new()?;
    codex_command(home.path())?
        .args(["git-credential", "get"])
        .write_stdin(format!(
            "protocol=https\nhost=chatgpt.com\n\n{SITES_GIT_REQUEST}"
        ))
        .assert()
        .success()
        .stdout("");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_credential_declines_personal_access_tokens_without_a_lookup() -> Result<()> {
    let home = TempDir::new()?;
    write_file_auth_config(home.path())?;
    let auth_api = MockServer::start().await;
    codex_command(home.path())?
        .env_remove(OPENAI_FEDERATION_RULE_ID_ENV_VAR)
        .env_remove(OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR)
        .env(CODEX_ACCESS_TOKEN_ENV_VAR, "at-fixture")
        .env("CODEX_AUTHAPI_BASE_URL", auth_api.uri())
        .args(["git-credential", "get"])
        .write_stdin(SITES_GIT_REQUEST)
        .assert()
        .success()
        .stdout("quit=true\n\n");
    assert!(auth_api.received_requests().await.unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn login_with_api_key_reads_stdin_and_writes_auth_json() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;

    codex_command(codex_home.path())?
        .args(["-c", "forced_login_method=\"api\"", "login"])
        .assert()
        .failure()
        .stderr(contains("--provider openai|claude_subscription"));
    codex_command(codex_home.path())?
        .args([
            "-c",
            "forced_login_method=\"api\"",
            "login",
            "--provider",
            "openai",
        ])
        .assert()
        .failure()
        .stderr(contains("ChatGPT login is disabled"));

    // Both streams must be terminals; invalid choices stay in the menu, not an OAuth flow.
    let mut env: std::collections::HashMap<String, String> = std::env::vars().collect();
    env.insert(
        "CODEX_HOME".into(),
        codex_home.path().to_string_lossy().into_owned(),
    );
    let codex_utils_pty::SpawnedProcess {
        session,
        mut stdout_rx,
        exit_rx,
        ..
    } = codex_utils_pty::spawn_pty_process(
        &codex_utils_cargo_bin::cargo_bin("codex")?.to_string_lossy(),
        &[
            "-c".into(),
            "forced_login_method=\"api\"".into(),
            "login".into(),
        ],
        codex_home.path(),
        &env,
        &None,
        codex_utils_pty::TerminalSize::default(),
        codex_utils_pty::ChildFds::Inherited(&[]),
    )
    .await?;
    let mut output = String::new();
    let mut sent = false;
    let code = tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(bytes) = stdout_rx.recv().await {
            output.push_str(&String::from_utf8_lossy(&bytes));
            if !sent && output.contains("Provider [1-2]:") {
                session
                    .writer_sender()
                    .send(b"invalid\n1\n".to_vec())
                    .await?;
                sent = true;
            }
        }
        Ok::<_, anyhow::Error>(exit_rx.await?)
    })
    .await??;
    assert_eq!(code, 1, "{output}");
    for message in [
        "ChatGPT (OpenAI)",
        "Claude subscription (Anthropic)",
        "Enter 1 for ChatGPT or 2 for Claude subscription.",
        "ChatGPT login is disabled",
    ] {
        assert!(output.contains(message), "{output}");
    }
    assert!(!output.contains("OpenRouter"));

    let mut cmd = codex_command(codex_home.path())?;
    let login = cmd
        .args([
            "-c",
            "forced_login_method=\"api\"",
            "login",
            "--with-api-key",
        ])
        .write_stdin("sk-test\n")
        .assert()
        .success();
    insta::assert_snapshot!(String::from_utf8_lossy(&login.get_output().stderr), @r"
    Reading API key from stdin...
    API key saved. Authentication is checked when you send a request.
    ");

    let auth = read_auth_json(codex_home.path())?;
    assert_eq!(auth["OPENAI_API_KEY"], "sk-test");
    assert!(auth.get("tokens").is_none());
    assert!(auth.get("agent_identity").is_none());

    let status = codex_command(codex_home.path())?
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .env_remove(OPENAI_FEDERATION_RULE_ID_ENV_VAR)
        .env_remove(OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR)
        .args(["login", "status"])
        .assert()
        .success();
    insta::assert_snapshot!(String::from_utf8_lossy(&status.get_output().stderr), @r"
    OpenAI (ChatGPT):
    API key configured - ***
    Authentication is checked when you send a request.
    Claude subscription (Anthropic):
    ");

    for args in [
        vec!["login", "--provider", "openai"],
        vec!["login", "--device-auth"],
    ] {
        codex_command(codex_home.path())?
            .args(["-c", "forced_login_method=\"api\""])
            .args(args)
            .assert()
            .failure()
            .stderr(contains("ChatGPT login is disabled"));
        assert_eq!(read_auth_json(codex_home.path())?, auth);
    }

    assert!(
        String::from_utf8_lossy(&status.get_output().stdout).contains("No saved Claude accounts.")
    );
    let claude = codex_login::anthropic::AccountStore::new(codex_home.path().to_path_buf())
        .upsert(
            codex_login::anthropic::AccountInfo {
                identity: codex_login::anthropic::AccountIdentity::new("claude-cli")?,
                email: Some("claude@example.test".into()),
                organization_uuid: None,
                organization_name: None,
            },
            codex_login::anthropic::SubscriptionGrant::bearer_only("synthetic-claude".into())?,
        )
        .await?;
    std::fs::remove_file(codex_home.path().join("auth.json"))?;
    codex_command(codex_home.path())?
        .args(["login", "status"])
        .assert()
        .success()
        .stderr(contains("Not logged in"))
        .stdout(contains(format!("{claude} · claude@example.test")));
    codex_command(codex_home.path())?
        .args(["login", "--provider", "openai", "status"])
        .assert()
        .failure()
        .stderr(contains("Not logged in"));
    codex_command(codex_home.path())?
        .args(["login", "--provider", "claude_subscription", "status"])
        .assert()
        .success()
        .stdout(contains("claude@example.test"));
    Ok(())
}

#[test]
fn api_key_auth_commands_work_when_log_dir_is_unavailable() -> Result<()> {
    let codex_home = TempDir::new()?;
    let log_dir = codex_home.path().join("unavailable-logs");
    std::fs::write(&log_dir, "not a directory")?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!("cli_auth_credentials_store = \"file\"\nlog_dir = {log_dir:?}\n"),
    )?;

    codex_command(codex_home.path())?
        .args(["login", "--with-api-key"])
        .write_stdin("sk-test\n")
        .assert()
        .success()
        .stderr(contains("Warning: failed to create login log directory"));
    assert_eq!(
        read_auth_json(codex_home.path())?["OPENAI_API_KEY"],
        "sk-test"
    );

    codex_command(codex_home.path())?
        .args(["login", "status"])
        .assert()
        .success()
        .stderr(contains("API key"));

    codex_command(codex_home.path())?
        .arg("logout")
        .assert()
        .success()
        .stderr(contains("Successfully logged out"));
    assert!(!codex_home.path().join("auth.json").exists());

    Ok(())
}

#[test]
fn login_status_reports_auth_storage_errors() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    std::fs::write(codex_home.path().join("auth.json"), "{invalid json")?;

    codex_command(codex_home.path())?
        .args(["login", "status"])
        .assert()
        .failure()
        .stderr(contains("Error checking OpenAI login status:"))
        .stdout(contains("No saved Claude accounts."));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quota_keeps_accounts_independent_without_switching_login() -> Result<()> {
    let server = MockServer::start().await;
    let home = TempDir::new()?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"{}/backend-api\"\n",
            server.uri()
        ),
    )?;
    for (id, email) in [("main", "a@example.com"), ("alt", "b@example.com")] {
        write_chatgpt_auth(
            home.path(),
            ChatGptAuthFixture::new(id)
                .account_id(id)
                .email(email)
                .plan_type("pro"),
            AuthCredentialsStoreMode::File,
        )?;
    }
    let before = read_auth_json(home.path())?;
    Mock::given(method("GET")).and(path("/backend-api/wham/usage"))
        .and(header("authorization", "Bearer main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type":"pro", "rate_limit": {"allowed":true,"limit_reached":false,
                "secondary_window":{"used_percent":27,"limit_window_seconds":604800,"reset_after_seconds":60,"reset_at":1800000000}}
        }))).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .and(header("authorization", "Bearer alt"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    let mut app_server = tokio::process::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?)
        .env("CODEX_HOME", home.path())
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .env_remove("OPENAI_API_KEY")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env("RUST_LOG", "info")
        .args(["app-server", "--listen", "unix://"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut startup = BufReader::new(app_server.stderr.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let line = startup
                .next_line()
                .await?
                .context("app-server exited before its control socket was listening")?;
            if line.contains("app-server control socket listening") {
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await
    .context("app-server control socket did not start listening within 5 s")??;
    let startup = tokio::spawn(async move {
        while startup.next_line().await?.is_some() {}
        Ok::<_, std::io::Error>(())
    });
    let output = codex_command(home.path())?
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .env_remove("OPENAI_API_KEY")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .arg("quota")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let result: Value = serde_json::from_slice(&output)?;
    assert_eq!(
        result["accounts"][0],
        json!({"id":":main","email":"a@example.com","remainingPercent":73.0,"resetsAt":1800000000,"error":null})
    );
    assert_eq!(result["accounts"][1]["remainingPercent"], Value::Null);
    assert!(result["accounts"][1]["error"].is_string());
    // Verify only after success: a mock Drop panic must not hide an earlier startup error.
    let requests = server.received_requests().await.context("quota requests")?;
    let mut authorization = requests
        .iter()
        .filter(|request| request.url.path() == "/backend-api/wham/usage")
        .map(|request| {
            assert_eq!(request.method.as_str(), "GET");
            assert_eq!(request.url.path(), "/backend-api/wham/usage");
            request.headers["authorization"]
                .to_str()
                .expect("authorization")
                .to_owned()
        })
        .collect::<Vec<_>>();
    authorization.sort();
    assert_eq!(authorization, ["Bearer alt", "Bearer main"]);
    assert_eq!(read_auth_json(home.path())?, before);
    app_server.kill().await?;
    startup.await??;
    Ok(())
}

#[test]
fn login_status_validates_configured_workload_identity() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    let missing_assertion = codex_home.path().join("missing-identity-token");

    for login_method in ["chatgpt", "api"] {
        let mut command = codex_command(codex_home.path())?;
        command
            .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
            .env(OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR, &missing_assertion)
            .env(OPENAI_FEDERATION_RULE_ID_ENV_VAR, "rule-test")
            .args([
                "-c",
                &format!("forced_login_method=\"{login_method}\""),
                "login",
                "status",
            ]);
        command
            .assert()
            .failure()
            .stderr(contains("could not read workload identity assertion file"));
    }

    Ok(())
}

#[tokio::test]
async fn logout_survives_enterprise_cleanup_failure_with_xaa_disabled() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(path("/backend-api/wham/config/bundle"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "config_toml": {"enterprise_managed": []},
            "requirements_toml": {"enterprise_managed": []},
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let home = TempDir::new()?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"{}/backend-api\"\n[features]\nuse_xaa = false\n[mcp_enterprise_managed_auth.idp]\nissuer = \"https://idp.example\"\nclient_id = \"enterprise-client\"\n[mcp_servers.enterprise]\nurl = \"https://resource.example/mcp\"\nauth = \"ema_auth\"\nbearer_token_env_var = \"UNUSED_TOKEN\"\n",
            server.uri()
        ),
    )?;
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("account-access")
            .account_id("workspace")
            .chatgpt_user_id("user"),
        AuthCredentialsStoreMode::File,
    )?;
    // Fail before touching the real keyring; the subprocess owns this isolated home.
    std::fs::write(home.path().join("mcp-oauth-locks"), "not a directory")?;
    for enabled in [false, true] {
        codex_command(home.path())?
            .current_dir(home.path())
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env_remove("CODEX_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
            .args([
                "-c",
                &format!("features.use_xaa={enabled}"),
                "mcp",
                "logout",
                "enterprise",
            ])
            .assert()
            .failure()
            .stderr(contains("failed to delete enterprise authorization"));
        assert!(home.path().join("auth.json").exists());
    }
    codex_command(home.path())?
        .current_dir(home.path())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env(
            REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR,
            format!("{}/oauth/revoke", server.uri()),
        )
        .env_remove("CODEX_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .args(["logout"])
        .assert()
        .success()
        .stderr(contains("continuing account logout"))
        .stderr(contains("Successfully logged out"));
    assert!(!home.path().join("auth.json").exists());
    Ok(())
}

#[test]
fn login_with_access_token_rejects_invalid_jwt() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["login", "--with-access-token"])
        .write_stdin("not-a-jwt\n")
        .assert()
        .failure()
        .stderr(contains("Error logging in with access token"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn debug_prompt_input_follows_authenticated_attribution_setting() -> Result<()> {
    let server = MockServer::start().await;
    let request_count = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/settings/user"))
        .and(header("chatgpt-account-id", "workspace-123"))
        .respond_with(move |_request: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(json!({
                "commit_attribution_enabled": request_count.fetch_add(1, Ordering::SeqCst) == 0,
            }))
        })
        .expect(2)
        .mount(&server)
        .await;
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"{}/backend-api\"\n",
            server.uri()
        ),
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("chatgpt-token")
            .account_id("workspace-123")
            .plan_type("enterprise"),
        AuthCredentialsStoreMode::File,
    )?;
    for enabled in [true, false] {
        let output = codex_command(codex_home.path())?
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env_remove("CODEX_ACCESS_TOKEN")
            .env_remove("OPENAI_API_KEY")
            .args(["debug", "prompt-input"])
            .output()?;
        assert!(output.status.success());
        let prompt = String::from_utf8(output.stdout)?;
        assert_eq!(
            prompt.contains("Co-authored-by: Codex <noreply@openai.com>"),
            enabled
        );
        assert!(!prompt.contains("attribution is disabled for the current workspace"));
    }
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_login_adds_an_account_without_revoking_existing_credentials() -> Result<()> {
    let new_home = TempDir::new()?;
    write_chatgpt_auth(
        new_home.path(),
        ChatGptAuthFixture::new("new-access")
            .chatgpt_account_id("new-account")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    let new_auth = read_auth_json(new_home.path())?;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_auth_id": "device-auth-123",
            "user_code": "CODE-12345",
            "interval": "0",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_code": "authorization-code-123",
            "code_challenge": "code-challenge-123",
            "code_verifier": "code-verifier-123",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id_token": new_auth["tokens"]["id_token"],
            "access_token": "new-access",
            "refresh_token": "new-refresh",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    std::fs::write(
        codex_home.path().join("auth.json"),
        serde_json::to_vec(&json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": "eyJhbGciOiJub25lIn0.e30.c2ln",
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "account_id": "old-account",
            },
        }))?,
    )?;

    let old_auth = read_auth_json(codex_home.path())?;
    let issuer = server.uri();
    let mut cmd = codex_command(codex_home.path())?;
    cmd.env(
        REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR,
        format!("{issuer}/oauth/revoke"),
    )
    .env("NO_PROXY", "127.0.0.1,localhost")
    .env("no_proxy", "127.0.0.1,localhost")
    .env_remove("CODEX_ACCESS_TOKEN")
    .env_remove("OPENAI_API_KEY")
    .args(["login", "--device-auth", "--experimental_issuer", &issuer])
    .assert()
    .success()
    .stderr(contains("Successfully logged in"));

    let requests = server
        .received_requests()
        .await
        .context("failed to read mock OAuth requests")?;
    let paths: Vec<&str> = requests.iter().map(|request| request.url.path()).collect();
    assert_eq!(
        paths,
        vec![
            "/api/accounts/deviceauth/usercode",
            "/api/accounts/deviceauth/token",
            "/oauth/token",
        ]
    );
    let auth = read_auth_json(codex_home.path())?;
    // The new login joins the saved accounts; the earlier login stays active.
    assert_eq!(auth["tokens"], old_auth["tokens"]);
    let added = &auth["wrangler_accounts"][":new-account"];
    assert_eq!(added["tokens"]["refresh_token"], "new-refresh");
    assert_eq!(added["tokens"]["account_id"], "new-account");

    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(400))
        .expect(1)
        .mount(&server)
        .await;
    codex_command(codex_home.path())?
        .env(
            REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR,
            format!("{issuer}/oauth/revoke"),
        )
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("OPENAI_API_KEY")
        .args(["login", "--device-auth", "--experimental_issuer", &issuer])
        .assert()
        .failure();
    assert_eq!(read_auth_json(codex_home.path())?, auth);
    Ok(())
}
