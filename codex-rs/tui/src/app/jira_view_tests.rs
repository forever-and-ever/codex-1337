use super::*;
use codex_app_server_protocol::JiraRevision;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

#[test]
fn jira_browser_surface_snapshot() {
    let mut browser = JiraBrowser::new(ThreadId::new());
    browser.project = "/workspace/project".into();
    browser.loading = false;
    let key = JiraKey { category: JiraCategory::Bug, slug: "slow-input".into() };
    browser.keys = vec![key.clone(), JiraKey { category: JiraCategory::Feature, slug: "new-panel".into() }];
    browser.selected = Some(key.clone());
    let mut terminal = Terminal::new(TestBackend::new(110, 22)).unwrap();
    let mut states = Vec::new();
    let issue = JiraIssue { revision: JiraRevision { key, epic: Some("latency".into()), version: "version".into() }, body: "Typing stalls when the worker is busy.\n\n- Keep **steering** responsive.\n- Preserve unrelated work.".into() };
    for state in 0..4 {
        match state {
            1 => browser.expanded = Some(issue.clone()),
            2 => browser.form = Some(JiraForm::new(Some(issue.clone()))),
            3 => browser.error = Some("Issue bug/slow-input already exists; choose another slug".into()),
            _ => {},
        }
        terminal.draw(|frame| { browser.render(frame.area(), frame.buffer_mut()); }).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height).map(|y| {
            (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_owned()
        }).collect::<Vec<_>>().join("\n");
        states.push(rendered);
    }
    insta::assert_snapshot!(states.join("\n\n---\n\n"));
    // A modal remains open across terminal resizes; cramped form geometry must
    // never index outside the buffer, even before the next full-size frame.
    for height in 0..18 {
        let area = Rect::new(0, 0, 80, height);
        browser.render(area, &mut Buffer::empty(area));
    }
}
