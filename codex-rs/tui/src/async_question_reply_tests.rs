use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn desktop_question_reply_accepts_single_and_batched_envelopes() {
    let reply = json!({"questionItemId": "[\"request_user_input_async\",\"item\",0]", "question": "Which environment?", "answer": "Staging", "extra": true});
    for payload in [reply.clone(), json!([reply])] {
        let text = format!(
            " \n<send_user_message_question_reply>\n{payload}\n</send_user_message_question_reply>\n "
        );
        assert_eq!(
            display_text(&text).as_deref(),
            Some("> Which environment?\n\nStaging")
        );
    }
    let text = "<send_user_message_question_reply>[{\"questionItemId\":\"one\",\"question\":\"First?\",\"answer\":\"Yes\"},{\"questionItemId\":\"two\",\"question\":\"Second?\",\"answer\":\"No\"}]</send_user_message_question_reply>";
    assert_eq!(
        display_text(text).as_deref(),
        Some("> First?\n\nYes\n\n> Second?\n\nNo")
    );
}

#[test]
fn coalescing_preserves_question_identity_attachments_and_latest_ide_context() {
    use codex_context_fragments::AnsweredQuestion;
    use codex_context_fragments::ContextualUserFragment;

    let first = AnsweredQuestion::new("one", "First?", "Yes").render();
    let second = AnsweredQuestion::new("two", "Second?", "No").render();
    let first = append(&first, &second).unwrap();
    let prefix =
        "# Context from my IDE setup:\n\n## Open tabs:\n- lib.rs\n\n## My request for Codex:\n";
    let input = vec![UserInput::Text {
        text: first,
        text_elements: Vec::new(),
    }];
    let skill = UserInput::Skill {
        name: "route".into(),
        path: "/tmp/route/SKILL.md".into(),
    };
    let image = UserInput::LocalImage {
        path: "/tmp/diagram.png".into(),
        detail: None,
    };
    let appended = vec![
        image.clone(),
        UserInput::Text {
            text: format!("{prefix}Actually, \"yes\" — use $route.\nSee diagram."),
            text_elements: Vec::new(),
        },
        skill.clone(),
    ];
    let merged = append_input(&input, &appended).unwrap();
    let UserInput::Text { text, .. } = &merged[0] else {
        panic!("reply envelope")
    };
    assert!(text.starts_with(prefix));
    assert_eq!(
        parse(text),
        Some(vec![
            AsyncQuestionReply {
                question_item_id: "one".into(),
                question: "First?".into(),
                answer: "Yes".into()
            },
            AsyncQuestionReply {
                question_item_id: "two".into(),
                question: "Second?".into(),
                answer: "No\nActually, \"yes\" — use $route.\nSee diagram.".into()
            },
        ])
    );
    assert_eq!(
        merged,
        vec![
            UserInput::Text {
                text: text.clone(),
                text_elements: Vec::new()
            },
            image,
            skill
        ]
    );
}

#[test]
fn malformed_or_embedded_question_envelopes_remain_ordinary_text() {
    for text in [
        "> Which environment?\n\nStaging",
        "<send_user_message_question_reply>[]</send_user_message_question_reply>",
        "<send_user_message_question_reply>[{\"questionItemId\":\"one\",\"question\":\"First?\",\"answer\":\"Yes\"},null]</send_user_message_question_reply>",
        "Quoted: <send_user_message_question_reply>{\"questionItemId\":\"one\",\"question\":\"First?\",\"answer\":\"Yes\"}</send_user_message_question_reply>",
        "<send_user_message_question_reply>{\"questionItemId\":\"one\",\"question\":\"First?\",\"answer\":\"Yes\"}</send_user_message_question_reply> trailing text",
    ] {
        assert_eq!(parse(text), None, "{text}");
    }
}
