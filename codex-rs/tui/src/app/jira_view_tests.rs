use super::*;
use codex_app_server_protocol::JiraResult;
use codex_app_server_protocol::JiraRevision;
use codex_app_server_protocol::ThreadJiraResponse;
use pretty_assertions::assert_eq;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

#[test]
fn jira_browser_surface_snapshot() {
    let mut browser = JiraBrowser::new(ThreadId::new());
    browser.project = "/workspace/project".into();
    browser.loading = false;
    let key = JiraKey {
        project: None,
        category: JiraCategory::Bug,
        slug: "slow-input".into(),
    };
    browser.keys = vec![
        key.clone(),
        JiraKey {
            project: Some(codex_utils_path_uri::LegacyAppPathString::from_string(
                "/workspace/project/child",
            )),
            category: JiraCategory::Feature,
            slug: "new-panel".into(),
        },
    ];
    browser.selected = Some(key.clone());
    let mut terminal = Terminal::new(TestBackend::new(/*width*/ 110, /*height*/ 22)).unwrap();
    let mut states = Vec::new();
    let issue = JiraIssue { revision: JiraRevision { key, epic: Some("latency".into()), version: "version".into() }, body: "Typing stalls when the worker is busy.\n\n- Keep **steering** responsive.\n- Preserve unrelated work.".into() };
    for state in 0..6 {
        match state {
            1 => browser.preview.result = Some(Ok(issue.clone())),
            2 => browser.form = Some(JiraForm::new(Some(issue.clone()))),
            3 => {
                browser.error =
                    Some("Issue bug/slow-input already exists; choose another slug".into())
            }
            4 => {
                let mut child = issue.clone();
                child.revision.key = browser.keys[1].clone();
                browser.form = Some(JiraForm::new(Some(child)));
                browser.error = None;
            }
            5 => {
                browser.form = None;
                browser.full_body = true;
                terminal.backend_mut().resize(/*width*/ 60, /*height*/ 20);
            }
            _ => {}
        }
        terminal
            .draw(|frame| {
                browser.render(frame.area(), frame.buffer_mut());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n");
        states.push(rendered);
    }
    insta::assert_snapshot!(states.join("\n\n---\n\n"));
    // A modal remains open across terminal resizes; cramped form geometry must
    // never index outside the buffer, even before the next full-size frame.
    for height in 0..18 {
        let area = Rect::new(/*x*/ 0, /*y*/ 0, /*width*/ 80, height);
        browser.render(area, &mut Buffer::empty(area));
    }
}

#[test]
fn jira_preview_tracks_hover_and_ignores_stale_reads() {
    let mut browser = JiraBrowser::new(ThreadId::new());
    browser.project = "/workspace".into();
    browser.scope = Some("scope".into());
    let first = JiraKey {
        project: None,
        category: JiraCategory::Bug,
        slug: "first".into(),
    };
    let second = JiraKey {
        slug: "second".into(),
        ..first.clone()
    };
    browser.keys = vec![first.clone(), second.clone()];
    browser.select(Some(first.clone()));
    browser.pending = Some((
        uuid::Uuid::new_v4(),
        JiraIntent::Preview { key: first.clone() },
    ));
    assert!(!browser.busy());
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 110, /*height*/ 22,
    );
    browser.render(area, &mut Buffer::empty(area));
    let row = browser.targets.rows[1].0;
    let hover = MouseEvent {
        kind: MouseEventKind::Moved,
        column: row.x,
        row: row.y,
        modifiers: KeyModifiers::NONE,
    };
    assert!(browser.mouse_input(hover));
    assert_eq!(browser.selected.as_ref(), Some(&second));
    assert!(!browser.mouse_input(hover));

    let issue = JiraIssue {
        revision: JiraRevision {
            key: second.clone(),
            epic: None,
            version: "v1".into(),
        },
        body: "Second issue body".into(),
    };
    // Selection may advance while a read is outstanding. Neither its success
    // nor its failure may populate the new issue's preview or suppress its read.
    browser.finish_preview(
        &first,
        Ok(ThreadJiraResponse {
            project: codex_utils_path_uri::LegacyAppPathString::from_string("/workspace"),
            scope: "scope".into(),
            result: JiraResult::Issue {
                issue: JiraIssue {
                    revision: JiraRevision {
                        key: first.clone(),
                        ..issue.revision.clone()
                    },
                    ..issue.clone()
                },
            },
        }),
    );
    browser.finish_preview(&first, Err("stale failure".into()));
    assert!(browser.preview.result.is_none());
    browser.finish_preview(
        &second,
        Ok(ThreadJiraResponse {
            project: codex_utils_path_uri::LegacyAppPathString::from_string("/workspace"),
            scope: "scope".into(),
            result: JiraResult::Issue {
                issue: issue.clone(),
            },
        }),
    );
    assert_eq!(browser.preview.issue(), Some(&issue));
    browser.select_neighbor(/*delta*/ -1);
    assert!(browser.preview.issue().is_none());

    // The wide-pane hit map must not survive a cramped resize.
    let tiny = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 18, /*height*/ 8,
    );
    browser.render(tiny, &mut Buffer::empty(tiny));
    assert!(!browser.mouse_input(hover));
}
