use super::AnthropicCachePolicy;
use super::history::History;
use serde_json::Value;
use serde_json::json;

pub(super) fn mark_prefixes(
    history: &mut History,
    tools: &mut [Value],
    policy: AnthropicCachePolicy,
) {
    let AnthropicCachePolicy::Ephemeral(ttl) = policy else {
        return;
    };
    let marker = json!({"type":"ephemeral", "ttl":ttl});
    // Native cache evaluation is tools -> system -> messages. Keep the stable
    // catalogue and instruction prefixes reusable when conversation content grows.
    for block in [tools.last_mut(), history.system.last_mut()]
        .into_iter()
        .flatten()
    {
        block["cache_control"] = marker.clone();
    }
    // The previous boundary survives an appended turn. The final boundary caches
    // warm repeats. Thinking blocks cannot carry cache markers; native compaction
    // can, without modifying its signed content or signature.
    let boundaries = history
        .messages
        .iter_mut()
        .rev()
        .filter_map(|message| {
            message.content.iter_mut().rev().find(|block| {
                matches!(
                    block["type"].as_str(),
                    Some("text" | "image" | "tool_use" | "tool_result" | "compaction")
                )
            })
        })
        .take(2);
    for block in boundaries {
        block["cache_control"] = marker.clone();
    }
}
