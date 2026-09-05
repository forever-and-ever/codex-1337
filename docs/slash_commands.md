# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

## Wrangler: `/questions`

Async questions appear above the composer without taking focus. `/questions`
opens the answer picker; Escape returns to the composer and Ctrl+D dismisses
the displayed questions without sending an answer. Submitted answers quote their
questions and enter the conversation as ordinary user messages, including during
a running turn. Pending state survives thread switches within this TUI, but is
not shared between clients or persisted across restarts.
