use super::*;

#[test]
fn status_version_follows_its_bundle_and_connected_server() {
    // Updating the server must not change either UI's independently assigned revision.
    insta::assert_snapshot!(
        [
            format_status_version("v0.157.0", Some(0)),
            format_status_version("v0.157.0", Some(1)),
            format_status_version("v0.158.0", Some(1)),
        ]
        .join("\n")
    );
}
