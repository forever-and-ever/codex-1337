use super::*;
use pretty_assertions::assert_eq;

#[test]
fn compact_patches_retain_full_changes_and_failure_details() {
    let patches = new_patch_events(
        HashMap::from([
            (
                PathBuf::from("new.txt"),
                FileChange::Add {
                    content: "one\ntwo\nthree\nfour\nfive\n".into(),
                },
            ),
            (
                PathBuf::from("existing.txt"),
                FileChange::Update {
                    unified_diff: "@@ -1,3 +1,3 @@\n context\n-before\n+after\n tail\n".into(),
                    move_path: None,
                },
            ),
        ]),
        Path::new("."),
    );
    let failure = patch_apply_cells(
        "failed".into(),
        HashMap::from([(
            PathBuf::from("new.txt"),
            FileChange::Add {
                content: "requested\n".into(),
            },
        )]),
        Path::new("."),
        PatchApplyStatus::Failed,
    )
    .remove(0);
    let missing = patch_apply_cells(
        "empty-failed".into(),
        HashMap::new(),
        Path::new("."),
        PatchApplyStatus::Failed,
    )
    .remove(0);
    let mut snapshots = Vec::new();
    let compact: Vec<_> = patches
        .iter()
        .flat_map(|patch| visible_lines(patch.compact_hyperlink_lines(/*width*/ 60)))
        .collect();
    let full: Vec<_> = patches
        .iter()
        .flat_map(|patch| visible_lines(patch.transcript_hyperlink_lines(/*width*/ 60)))
        .collect();
    snapshots.push(format!(
        "patch\nCompact\n{}\nFull\n{}",
        Text::from(compact),
        Text::from(full)
    ));
    for (label, cell) in [
        ("failure", &failure as &dyn HistoryCell),
        ("missing diagnostics", &missing),
    ] {
        let compact = visible_lines(cell.compact_hyperlink_lines(/*width*/ 40));
        let full = visible_lines(cell.transcript_hyperlink_lines(/*width*/ 40));
        snapshots.push(format!(
            "{label}\nCompact\n{}\nFull\n{}",
            Text::from(compact),
            Text::from(full)
        ));
    }
    insta::assert_snapshot!(snapshots.join("\n\n"));
}

#[test]
fn viewed_image_retains_original_path_in_details() {
    for path in [
        "/workspace/assets/example.png",
        r"C:\workspace\assets\example.png",
    ] {
        let cell = new_view_image_tool_call(
            LegacyAppPathString::from_string(path),
            ImagePathOrigin::Local,
        );
        assert_eq!(
            cell.display_lines(/*width*/ 80)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["• Viewed image example.png"]
        );
        assert_eq!(
            cell.raw_lines(),
            vec![Line::from(format!("Viewed image {path}"))]
        );
        assert_eq!(
            cell.transcript_lines(/*width*/ 200)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![format!("• Viewed image {path}")]
        );
    }
}

#[test]
fn viewed_image_narrow_summary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("very-long-screenshot #5.png");
    image::RgbaImage::new(1, 1).save(&path).unwrap();
    let url = Url::from_file_path(path.canonicalize().unwrap()).unwrap();
    let cell = new_view_image_tool_call(
        LegacyAppPathString::from_path(&path),
        ImagePathOrigin::Local,
    );
    cell.link_local_image();
    let compact = cell.compact_hyperlink_lines(/*width*/ 32);
    let expanded = cell.expanded_hyperlink_lines(u16::MAX);
    for row in [&compact[0], &expanded[0]] {
        let [link] = row.hyperlinks.as_slice() else {
            panic!("expected one image link")
        };
        assert_eq!(
            (link.columns.clone(), link.terminal_destination()),
            (15..row.width(), Some(url.to_string()))
        );
    }
    insta::assert_snapshot!(compact[0].line.to_string());
}
