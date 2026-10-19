# Live captures

Four frames of one fabricated conversation, driven through the App as the app server drives
it, drawn full screen at 100×60 by the TUI's own renderers and exported before warpaint binds
them. `just warpaint-capture` regenerates them from `codex-rs/tui/src/app/warpaint_capture.rs`;
a rerun is byte-identical.

- `conversation.json`: the transcript as it opens: the prompt, a failed and a passing command
  with their inputs, the patch opened, a subagent's message, the answer (heading, emphasis,
  table, list, inline code, link, highlighted Rust), its reasoning opened from the response
  header (a list with inline code), the completion line, the warning badge, a draft in the
  composer and the footer.
- `detail.json`: the same transcript in detail (`ctrl+t`): command output in its terminal
  colors, exit marks, the warning inline and the reasoning.
- `selection.json`: `/copy` holding the latest response.
- `picker.json`: `/theme` over the conversation, previewing a diff.

A color is named as the binder resolves it, short of the pack: tokens by role (`ground:`,
`figure:`, `syntax:`), upstream's named chrome colors by the role they stand for, dimmed ink
as `figure:ink_faint`, tool output as `terminal:<index>`, and `reset` for the page's ink and
canvas. The exporter proves every cell's names against the real binder.

Not exercised: `ground:dais` (proposed plans), `figure:link`, heat (only `/usage` draws the
activity chart), livery (thread identities appear only in multi-agent views), and the terminal
ramp beyond red, green and yellow. `render --audit` marks the RGB literals fork chrome still
paints: entry titles and tabs (`render/header.rs`) and token figures
(`transcript_view/text_gutter.rs`).
