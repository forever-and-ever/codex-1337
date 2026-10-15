use super::*;
use codex_protocol::ToolName;
use codex_protocol::tool_input::ToolInput;
use codex_protocol::tool_input::ToolInputPayload;
use pretty_assertions::assert_eq;

#[test]
fn literal_reads_exclude_composition_and_expansion() {
    for command in [
        "cat file | head",
        "cat file; cat other",
        "cat $FILE",
        "cat *.rs",
        "sed -i '1,2p' file",
        "cat a b",
    ] {
        assert!(ReadIntent::parse(command).is_none(), "{command}");
    }
    let read = ReadIntent::parse("sed -n '12,24p' 'a b.rs'").unwrap();
    assert_eq!(
        read,
        ReadIntent {
            path: "a b.rs".into(),
            range: ReadRange::Lines {
                first: 12,
                last: 24
            }
        }
    );
    // Filesystem helpers lower to actual tool inputs, independent of JavaScript syntax.
    let mut input = ToolInput {
        id: "input".into(),
        call_id: "read".into(),
        name: ToolName::plain("fs"),
        payload: ToolInputPayload::Json(String::new()),
    };
    for arguments in [
        r#"{"operation":"read","path":"/a b/$file.rs","encoding":"utf8"}"#,
        r#"{"operation":"read","path":"/a b/$file.rs","environment_id":"remote"}"#,
    ] {
        input.payload = ToolInputPayload::Json(arguments.into());
        assert_eq!(
            ReadIntent::from_tool_input(&input),
            Some(ReadIntent {
                path: "/a b/$file.rs".into(),
                range: ReadRange::Whole,
            })
        );
    }
    for arguments in [
        r#"{"operation":"write","path":"file","data":"text"}"#,
        r#"{"operation":"read","path":""}"#,
        r#"{"operation":"read","path":42}"#,
        r#"{"path":"file"}"#,
        "not JSON",
    ] {
        input.payload = ToolInputPayload::Json(arguments.into());
        assert!(ReadIntent::from_tool_input(&input).is_none(), "{arguments}");
    }
    let arguments = r#"{"operation":"read","path":"file"}"#;
    input.payload = ToolInputPayload::Text(arguments.into());
    assert!(ReadIntent::from_tool_input(&input).is_none());
    input.payload = ToolInputPayload::Json(arguments.into());
    input.name = ToolName::namespaced("other", "fs");
    assert!(ReadIntent::from_tool_input(&input).is_none());
    input.name = ToolName::plain("other");
    assert!(ReadIntent::from_tool_input(&input).is_none());
}
