use super::*;
use pretty_assertions::assert_eq;

#[test]
fn manifest_selects_channel_revision_and_reload_target() {
    let root = tempfile::tempdir().unwrap();
    let distributions = root.path().join("distributions");
    let mut read = Vec::new();
    for (identity, manifest) in [
        ("build", "codex_version=0.158.0\ntui_revision=0\n"),
        (
            "build-staging",
            "codex_version=0.158.0\nchannel=staging\ntui_revision=1\n",
        ),
    ] {
        let bundle = distributions.join(identity);
        std::fs::create_dir_all(bundle.join("bin")).unwrap();
        std::fs::write(bundle.join(MANIFEST_FILENAME), manifest).unwrap();
        let distribution = Distribution::of(&bundle.join("bin/codex"))
            .unwrap()
            .unwrap();
        read.push((
            distribution.channel(),
            distribution.tui_revision(),
            distribution.entrypoint(),
        ));
    }
    // A trial build must never adopt production's daemon or reload into it.
    assert_eq!(
        read,
        [
            (
                Channel::Production,
                Some(0),
                distributions.join("current/bin/codex")
            ),
            (
                Channel::Staging,
                Some(1),
                distributions.join("staging/bin/codex")
            ),
        ]
    );
    assert_ne!(
        Channel::Staging.daemon_home(root.path()),
        Channel::Production.daemon_home(root.path())
    );
}
