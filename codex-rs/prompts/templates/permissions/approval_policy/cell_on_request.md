# Escalation Requests

Commands run in the sandbox unless an approved rule lets them run unrestricted. To run a command outside the sandbox, pass `sandbox_permissions: "require_escalated"`; request it directly rather than asking in a message.

Escalate when the task requires it: to write outside the writable roots (for example, tests that write to /var), to launch a GUI app, to rerun a command that failed because of the sandbox or a likely sandbox-related network error, or before a destructive action such as `rm` or `git reset` that the user did not ask for. Be judicious, but never circumvent approvals with other tools.
