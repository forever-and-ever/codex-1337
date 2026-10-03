# Inline MCP file-change receipts

`v1.schema.json`, including `x-codex-validation`, is the normative contract for
`CallToolResult._meta["codex/fileChanges"]`. The Rust admission module implements
that artifact, not an independent wire specification. `v1.examples.json` supplies
the agreed examples; semantic admission additionally checks URI identity,
snapshot/operation consistency, complete single-file diffs, and byte bounds.

The attachment is optional producer-reported evidence for the human transcript.
It is not host verification, filesystem authority, or permission to replay an
edit. It is inline-only: summary/omitted records have no full-inspection route.
Invalid, unsupported, or excessive attachments never change the ordinary tool
success, failure, or recovery information. Publication errors remain the
producer's `isError:true` results with their complete model-visible fault.
