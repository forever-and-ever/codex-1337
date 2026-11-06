# Configuration

For basic configuration instructions, see [this documentation](https://developers.openai.com/codex/config-basic).

For advanced configuration instructions, see [this documentation](https://developers.openai.com/codex/config-advanced).

For a full configuration reference, see [this documentation](https://developers.openai.com/codex/config-reference).

## Lifecycle hooks

Admins can set top-level `allow_managed_hooks_only = true` in
`requirements.toml` to ignore user, project, and session hook configs while
still allowing managed hooks from requirements and managed config layers. This
setting is only supported in `requirements.toml`; putting it in `config.toml`
does not enable managed-hooks-only mode.

### Transcript path

A hook input's `transcript_path` names the thread's rollout file on disk. A
rollout written by this build is zstd-compressed JSONL (`.jsonl.zst`), a
sequence of standard zstd frames that `zstd -dc` decodes. Older rollouts that
have not been converted are plain `.jsonl`. While the thread is running, its
last frame is still open: decoding yields every line recorded so far, and then
`zstd` reports that the input is truncated.

### Hook selectors

Each hook group accepts an optional typed `selector`. For `PreToolUse`,
`PermissionRequest`, and `PostToolUse`, select the namespace and tool name
independently:

```toml
[[hooks.PreToolUse]]
selector = { type = "tool", namespace = "^functions$", name = "^exec_command$" }

[[hooks.PreToolUse.hooks]]
type = "command"
command = "/home/me/bin/check-command"
```

Both supplied patterns must match. An omitted component matches any namespace
or name; the default function namespace is `functions`. Names are the actual
tool names, not aliases or flattened namespace/name strings.
Omitting the entire selector matches all calls for that event, including calls
without an attributed tool. An explicit `selector = { type = "tool" }` matches
only calls with an attributed tool.

Lifecycle selectors use `event_value`, matching the event's source, reason,
trigger, or agent type rather than its event name:

```toml
[[hooks.SessionStart]]
selector = { type = "event_value", pattern = "^startup$" }

[[hooks.SessionStart.hooks]]
type = "command"
command = "/home/me/bin/prepare-session"
```

`event_value` is accepted for `SessionStart`, `SessionEnd`, `PreCompact`,
`PostCompact`, `SubagentStart`, and `SubagentStop`. `UserPromptSubmit`, `Stop`,
and `Interrupt` do not accept selectors. Tool selectors on lifecycle events
and lifecycle selectors on tool events are invalid.

Patterns accept regular expressions; plain names and `|`-separated plain names
match exactly. An empty pattern or `*` matches any value. Use anchors for an
exact match when a pattern contains regular-expression syntax.

JSON hook files use the same tagged selector objects. Selector objects and
hook groups reject unknown fields; selectors must use the declared object shape.

Tool-event command inputs carry `tool_name` as a structured address, for example
`{"namespace":"functions","name":"exec_command"}`. A `PermissionRequest` without
an attributable invocation carries `tool_name: null`. When a later `write_stdin`
call observes execution completion, `PostToolUse` retains the original execution
address and `tool_use_id`, not the polling call\'s identity.

External hook imports preserve unfiltered groups and lifecycle event-value
filters. A foreign tool filter without a representable namespace/name split
rejects the hooks import before writing its configuration. Other selected import
items report their outcomes independently.
