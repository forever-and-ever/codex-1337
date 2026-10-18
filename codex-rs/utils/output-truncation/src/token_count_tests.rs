use super::*;
use pretty_assertions::assert_eq;

#[test]
fn tool_text_matches_reference_tokenizer_without_interpreting_control_markers() {
    // Independent fixtures from OpenAI tiktoken 0.14, encode_ordinary/o200k_base.
    // Literal special-token spellings in untrusted tool output must not become control tokens.
    let samples = [
        ("hello world", 2),
        ("fn main() { println!(\"你好 🦀\"); }\n", 12),
        ("<|endoftext|> <|im_start|>", 13),
    ];
    for (text, expected) in samples {
        assert_eq!(
            count_o200k_text_tokens(&FunctionCallOutputBody::Text(text.into())),
            Some(expected)
        );
        assert_eq!(
            count_o200k_arrival(&ArrivalSource::Captured {
                blocks: vec![text.into()],
            }),
            Some(expected)
        );
        assert_eq!(
            count_o200k_arrival(
                &ArrivalSource::ToolDeclarations {
                    text: text.into(),
                }
            ),
            Some(expected)
        );
    }
}
