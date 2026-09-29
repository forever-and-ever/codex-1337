use super::*;
use pretty_assertions::assert_eq;

#[test]
fn status_version_follows_its_bundle_and_connected_server() {
    let packages = tempfile::tempdir().unwrap();
    let mut versions = Vec::new();
    for revision in 0..2 {
        let bundle = packages.path().join(format!("build-{revision}"));
        std::fs::create_dir_all(bundle.join("bin")).unwrap();
        std::fs::write(
            bundle.join("distribution"),
            format!("codex_version=0.158.0\ntui_revision={revision}\n"),
        )
        .unwrap();
        versions.push(bundle.join("bin/codex"));
    }
    // Installing a newer UI does not relabel the older executable, and updating
    // the server must not change either UI's independently assigned revision.
    let old = read_tui_revision(&versions[0]).unwrap();
    let new = read_tui_revision(&versions[1]).unwrap();
    assert_eq!((old, new), (Some(0), Some(1)));
    insta::assert_snapshot!(
        [
            format_status_version("v0.157.0", old),
            format_status_version("v0.157.0", new),
            format_status_version("v0.158.0", new),
        ]
        .join("\n")
    );
}
