//! Root metadata protections, driven through a hermetic outer bubblewrap root
//! whose fresh runtime directory stands in for a new boot.

use super::codex_linux_sandbox_exe;
use super::should_skip_bwrap_tests;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSandboxPolicyContext;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

/// A user-namespace root, running as `uid`, with system binaries and an empty
/// runtime directory.
fn hermetic_root(bwrap: &Path, uid: u32) -> Command {
    let mut command = Command::new(bwrap);
    command.args(
        format!("--unshare-user --unshare-pid --uid {uid} --gid {uid} --die-with-parent --tmpfs /")
            .split_whitespace(),
    );
    command
        .args("--ro-bind /usr /usr --ro-bind /etc /etc --proc /proc --dev /dev".split_whitespace());
    command.args(
        format!("--dir /tmp --dir /run --dir /run/user --perms 0700 --dir /run/user/{uid}")
            .split_whitespace(),
    );
    for path in ["/bin", "/sbin", "/lib", "/lib64"] {
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if metadata.file_type().is_symlink() {
                command
                    .arg("--symlink")
                    .arg(fs::read_link(path).unwrap())
                    .arg(path);
            } else {
                command.args(["--ro-bind", path, path]);
            }
        }
    }
    command
}

fn is_namespace_failure(stderr: &str) -> bool {
    stderr.starts_with("bwrap: Creating new namespace failed")
        || stderr.contains("No permissions to create a new namespace")
        || stderr.contains("setting up uid map")
}

#[tokio::test]
async fn root_metadata_symlinks_cannot_reopen_approved_denials() {
    if should_skip_bwrap_tests().await {
        return;
    }
    let Some(bwrap) = codex_sandboxing::find_system_bwrap_in_path(
        &PermissionProfile::read_only().file_system_sandbox_policy(),
        &std::env::current_dir().expect("current directory"),
    ) else {
        eprintln!("skipping root metadata test: system bubblewrap is unavailable");
        return;
    };
    let fixture = tempfile::tempdir_in("/tmp").unwrap();
    let work = fixture.path().join("work");
    let private = fixture.path().join("private");
    fs::create_dir_all(work.join("public")).unwrap();
    fs::create_dir_all(private.join("reopened")).unwrap();
    fs::write(work.join("public/secret"), "outside").unwrap();
    fs::write(private.join("reopened/secret"), "private").unwrap();
    fs::write(private.join("sibling"), "sibling").unwrap();
    let sandbox_exe = codex_linux_sandbox_exe();
    codex_utils_cargo_bin::copy_executable(&sandbox_exe, &work.join("sandbox")).unwrap();
    // Carry the bundled fallback into the isolated root. Cargo builds without
    // an embedded digest can use the system binary when no bundle is available.
    let fixture_bwrap = codex_linux_sandbox::find_bundled_bwrap_for_exe(
        &sandbox_exe,
        &std::env::current_dir().expect("current directory"),
        |key| std::env::var_os(key),
    )
    .map(AbsolutePathBuf::into_path_buf)
    .or_else(|| {
        option_env!("CODEX_BWRAP_SHA256")
            .is_none_or(|digest| digest.chars().all(|character| character == '0'))
            .then(|| bwrap.clone())
    })
    .expect("digest-verified builds require the matching bundled bubblewrap");
    let resources = work.join("codex-resources");
    fs::create_dir(&resources).unwrap();
    codex_utils_cargo_bin::copy_executable(&fixture_bwrap, &resources.join("bwrap")).unwrap();

    let path_entry = |path: &str, access| {
        FileSystemSandboxEntry::new(
            AbsolutePathBuf::from_absolute_path(path).unwrap().into(),
            access,
        )
    };
    let cwd = AbsolutePathBuf::from_absolute_path("/tmp").unwrap().into();
    let context = FileSystemSandboxPolicyContext {
        cwd: &cwd,
        workspace_roots: std::slice::from_ref(&cwd),
        user_home_dir: None,
        temporary_directories: Some(&[]),
    };
    let approved = |denied| {
        let mut policy = FileSystemSandboxPolicy::read_only();
        policy
            .entries
            .push(path_entry(denied, FileSystemAccessMode::Deny));
        policy.for_approved_command(&context)
    };
    let run = |policy: &FileSystemSandboxPolicy,
               private_mount: &str,
               codex_target: &str,
               script: &str| {
        let profile =
            PermissionProfile::from_runtime_permissions(policy, NetworkSandboxPolicy::Enabled);
        let mut command = hermetic_root(&bwrap, /*uid*/ 0);
        command.args("--dir /.agents --symlink /private-link /.git".split_whitespace());
        command.args(["--symlink", codex_target, "/.codex"]);
        command.args(["--symlink", "/private/reopened", "/private-link"]);
        command.arg("--bind").arg(&work).arg("/tmp");
        command.arg("--bind").arg(&private).arg(private_mount);
        if private_mount != "/private" {
            command.args(["--symlink", private_mount, "/private"]);
        }
        command.args(
            "--chdir /tmp --setenv TMPDIR /tmp --setenv PATH /usr/bin:/bin".split_whitespace(),
        );
        command.args(
            "-- /tmp/sandbox --sandbox-policy-cwd /tmp --permission-profile".split_whitespace(),
        );
        command.arg(serde_json::to_string(&profile).unwrap());
        command.args(["--", "/bin/sh", "-c", script]);
        command.output().unwrap()
    };

    let private_denied = approved("/private");
    let script = r#"
        set -eu
        if cat /private/reopened/secret >/dev/null 2>&1 ||
           cat /.codex/secret >/dev/null 2>&1 || cat /.git/secret >/dev/null 2>&1; then exit 21; fi
        if (printf bad > /.codex/changed) 2>/dev/null; then exit 22; fi
        printf good > /.agents/allowed
        printf good > /tmp/marker
        printf 'root-metadata-denials-held\n'
    "#;
    let output = run(&private_denied, "/private", "/private/reopened", script);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if is_namespace_failure(&stderr) {
        eprintln!("skipping root metadata test: {stderr}");
        return;
    }
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"root-metadata-denials-held\n");
    assert_eq!(fs::read_to_string(work.join("marker")).unwrap(), "good");
    assert!(!private.join("reopened/changed").exists());

    let script = r#"
        set -eu
        printf safe > /.codex/allowed
        if cat /.git/secret >/dev/null 2>&1; then exit 23; fi
    "#;
    let output = run(&private_denied, "/private", "/tmp/public", script);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        fs::read_to_string(work.join("public/allowed")).unwrap(),
        "safe"
    );

    let script = r#"
        set -eu
        rm /private
        ln -s /tmp/public /private
        cat /.codex/secret
    "#;
    let output = run(&private_denied, "/real-private", "/private", script);
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains(
        "cannot enforce sandbox deny-read path /private because it crosses writable symlink /private"
    ), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");

    let mut reopened = private_denied;
    reopened
        .entries
        .push(path_entry("/private/reopened", FileSystemAccessMode::Write));
    let script = r#"
        set -eu
        test "$(cat /.codex/secret)" = private
        if cat /private/sibling >/dev/null 2>&1; then exit 24; fi
    "#;
    let output = run(&reopened, "/private", "/private/reopened", script);
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    let output = run(
        &approved("/.codex/secret"),
        "/private",
        "/private/reopened",
        "true",
    );
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains(
        "cannot enforce sandbox deny-read path /.codex/secret because it crosses writable symlink /.codex"
    ), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}

/// Cleanup of a missing metadata path's mountpoint runs only when the wrapper
/// exits normally. Any later run of the boot must reap it, even in a root it
/// does not use; after a reboot erases the registry, a run that reuses the root
/// must reap it.
#[tokio::test]
async fn mountpoints_of_killed_sandboxes_are_reaped_by_later_runs() {
    if should_skip_bwrap_tests().await {
        return;
    }
    let Some(bwrap) = codex_sandboxing::find_system_bwrap_in_path(
        &PermissionProfile::read_only().file_system_sandbox_policy(),
        &std::env::current_dir().expect("current directory"),
    ) else {
        eprintln!("skipping mountpoint reaping test: system bubblewrap is unavailable");
        return;
    };
    let fixture = tempfile::tempdir_in("/tmp").unwrap();
    for dir in ["first", "second"] {
        fs::create_dir(fixture.path().join(dir)).unwrap();
    }
    codex_utils_cargo_bin::copy_executable(
        &codex_linux_sandbox_exe(),
        &fixture.path().join("sandbox"),
    )
    .unwrap();
    let profile = |root: &str| {
        let root = AbsolutePathBuf::from_absolute_path(root).unwrap();
        serde_json::to_string(&PermissionProfile::workspace_write_with(
            &[root],
            NetworkSandboxPolicy::Enabled,
            /*exclude_tmpdir_env_var*/ true,
            /*exclude_slash_tmp*/ true,
        ))
        .unwrap()
    };
    // Each boot gets a fresh runtime registry. Keeping the caller's uid lets
    // the test run inside an outer sandbox.
    let uid = fs::metadata(fixture.path()).unwrap().uid();
    let boot = |script: &str| {
        let output = hermetic_root(&bwrap, uid)
            .arg("--bind")
            .arg(fixture.path())
            .arg("/fx")
            .args(["--setenv", "PATH", "/usr/bin:/bin"])
            .args(["--", "/bin/bash", "-c"])
            .arg(format!("{KILL_AND_REAP}{script}"))
            .args(["boot", &profile("/fx/first"), &profile("/fx/second")])
            .output()
            .unwrap();
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };

    let (status, stderr) = boot("kill_mid_run; reap; kill_mid_run");
    if is_namespace_failure(&stderr) {
        eprintln!("skipping mountpoint reaping test: {stderr}");
        return;
    }
    assert_eq!(status, Some(0), "{stderr}");
    let probe = fixture.path().join("probe");
    fs::write(&probe, "").unwrap();
    if rustix::fs::setxattr(&probe, "user.probe", b"", rustix::fs::XattrFlags::empty()).is_err() {
        eprintln!("skipping reboot reaping: the fixture filesystem lacks user xattrs");
        return;
    }
    let (status, stderr) = boot("reuse");
    assert_eq!(status, Some(0), "{stderr}");
}

const KILL_AND_REAP: &str = r#"
set -eu
first=$1 second=$2
# SIGKILL a wrapper mid-run, as a timeout or interrupt does, and wait for its
# sandbox to die; the mountpoints it created stay behind.
kill_mid_run() {
    rm -f /fx/pipe
    mkfifo /fx/pipe
    /fx/sandbox --sandbox-policy-cwd /fx/first --permission-profile "$first" \
        -- /bin/sh -c 'echo ready; exec sleep 60' > /fx/pipe &
    wrapper=$!
    exec 3< /fx/pipe
    read -r _ <&3
    kill -KILL "$wrapper"
    cat <&3 > /dev/null
    exec 3<&-
    test -d /fx/first/.git || exit 21
}
reap() {
    /fx/sandbox --sandbox-policy-cwd /fx/second --permission-profile "$second" -- /bin/true
    test -z "$(ls -A /fx/first)" || exit 22
    test -z "$(ls -A /fx/second)" || exit 23
}
reuse() {
    /fx/sandbox --sandbox-policy-cwd /fx/first --permission-profile "$first" -- /bin/true
    test -z "$(ls -A /fx/first)" || exit 24
}
"#;
