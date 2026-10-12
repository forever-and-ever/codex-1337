# Native Anthropic differential corpus

`corpus.jsonl` contains eight synthetic cases; each case's `expect` holds the
native contract. No fixture contains a real conversation, credential or upstream
response.

## Adapter contract

Read one JSON object per input line and emit one capture per line, with the same
`id`. Each case supplies `model`, a synthetic Responses-shaped `request`, and
Anthropic `frames`. These are fixture inputs, not a production HTTP boundary.
Convert request history into canonical `ResponseItem`s and tools into native
wire tools before calling the Rust encoder. Feed frames to the Rust decoder as
`event: <frame.type>\ndata: <frame JSON>\n\n`. Replay whole frames and split the
same bytes across arbitrary transport chunks, including within UTF-8, JSON
escapes, `data:` and the blank-line separator. No inference is required.

Each capture exposes these projections:

| Field | Meaning |
| --- | --- |
| `id` | Exact fixture identity |
| `request` | Native subscription Messages body |
| `output` | Canonical history items reconstructed from the stream |
| `usage` | `uncached_input`, `cache_write`, `cache_read`, `output` integers |
| `terminal` | `completed`, `incomplete`, `error`, or `truncated` |

The native adapter may serialize existing `ResponseItem`s into `output`; this
does not require a Responses emulation endpoint. Incomplete output remains
available. An error event is an error, and EOF without `message_stop` is
truncation, not successful completion. Both must end the request without replay.

The checker compares explicit JSON pointers in `expect`. It normalizes only a
Messages scalar text body into one text block. It never trims text, sorts arrays,
rewrites arguments, strips signatures, moves cache markers, or normalizes tool
identity. Generated item IDs and unrelated optional fields are not asserted.
The native tool policy keeps these legal aliases (`functions__exec` and
`send_message`); adapters need no parallel alias algorithm or fixture alias map.
Canonical `ContentItem` carries no `cache_control` annotation, so native
assertions check system text and order separately from the typed request-wide
cache policy.

```sh
~/.local/libexec/cpu-lanes uv run --script scripts/native_anthropic/check.py native-capture.jsonl
```

The native adapter is the `codex-api` development example `native_anthropic_replay`.
From `codex-rs`, run:

```sh
cargo run -p codex-api --example native_anthropic_replay -- \
  ../scripts/native_anthropic/models.json \
  < ../scripts/native_anthropic/corpus.jsonl > native-capture.jsonl
```

`models.json` supplies synthetic canonical `ModelInfo` records for wire validation,
not a second production model catalogue or an upstream capability parser. Native
catalogue projection is validated separately. The adapter calls the public native
encoder and `AnthropicClient`; its transport accepts exactly one synthetic
Messages POST per replay and cannot perform network I/O. Authorization and cookie
headers are forbidden. The request body must equal the encoder's captured body.
Whole SSE frames and one-, seven-, and 31-byte chunks must produce identical
captures, including exact signed content and argument text. Each case has a
ten-second deadline. No checker exemptions are permitted.
Native function schemas use the production `arguments` envelope projector.
The corpus's canonical function SSE is enveloped only inside the native
simulated transport, including its original fragmented JSON text; the decoder
must recover identical canonical arguments. Custom `{input}` remains unchanged.
Golden pointers, signatures and canonical outputs are never rewritten.
`event_trace` retains native event diagnostics so fragmentation equality also
checks intermediate text, tool-input deltas and reasoning events, not only final
history. This trace is compared within one adapter binary, not used as a stable
protocol fixture.

## Opt-in live acceptance

`codex-model-provider`'s `native_anthropic_acceptance` example is a separate live
probe. Without `--run` it prints its limits and sends nothing. An operator must
explicitly supply the actual account UUID, an advertised signed-compaction model,
a fresh receipt directory, and the current subscription bearer on standard input:

```sh
# From codex-rs; supply the bearer through an operator-owned private stdin pipe.
cargo run -p codex-model-provider --example native_anthropic_acceptance -- \
  --run ACCOUNT_UUID COMPACTION_MODEL FRESH_RECEIPT_DIR
```

Use `--run-signed` instead of `--run` for a **new**, independent signed-only
attempt: four Messages POSTs, no Haiku/cache-TTL replay or six-minute wait,
signed inference capped at 512 and compaction still capped at 4096, inside a
four-minute overall deadline. It requires
a fresh receipt directory and independent synthetic prefix. This is explicit
operator selection, not an automatic retry of an unsuccessful earlier attempt.
For Sonnet 5.5 the actual declared `Low` effort uses that 512-token inference cap;
no capability is inferred from a rejection or rate-limit error.

The example uses a private `TempDir` registry with a bearer-only grant. It reads
no production account/configuration file, has no refresh token, never invokes
refresh, rejects redirects, and performs no automatic retries or continuation.
Production refresh ownership is unaffected. Its real native catalogue route
projects upstream capabilities; `actual-models.json` preserves that authority.
Discovery is bounded to ten pages of at most 1 MiB each, the existing five-second
catalogue deadline, and zero transport retries. No model fallback is accepted.
All four signed stages use `None` only when the actual canonical model record
declares it, otherwise the declared native `Low` effort. Explicitly selecting
`claude-sonnet-5` permits the bounded no-thinking proof; Sonnet 5.5 does not
declare disabled thinking and its adaptive reasoning shares the output cap.
The selected effort is included in each started receipt.

The full arm has at most thirteen Messages POSTs, each with a sixty-second deadline,
inside a twenty-minute overall deadline. Nine Haiku calls have an exact output
cap of 64; three signed-model inference calls have an exact cap of 128. Explicit
compaction uses the protocol's `AnthropicSignedCompaction::MAX_OUTPUT_TOKENS`
ceiling of 4096. A cap hit is a failed receipt, not permission to increase the cap or
replay. Request and persisted-history bodies are bounded to 256 KiB. The inert
128-record prefix is a deliberate >1K-token fixture requiring manual context-size
review, bounded to 128 fixed records and less than 64 KiB per text item. These
are fixed-fixture and encoded-byte bounds, not a tokenizer guarantee. Upstream
charged input can aggregate internal compaction iterations and is not treated
as the size of a single context item. Cache admission
is established by actual nonzero cold cache writes, not a character-count token
estimate. No blanket cache-hit percentage is claimed.

The ordered flow is parent one-hour cold/warm, append a tiny declared synthetic
tool, supply its fixed `OK` result, serialize and reload canonical history, child
five-minute cold/warm, wait 360 seconds, repeat the retained parent and expired
child, then an independently prefixed signed-model cold inference, explicit
compaction, persisted signed checkpoint reload, an `echo_fixture({})` request,
and a final model reply after persisting/reloading the canonical tool call and
its fixed `OK` result. Both tool arms require the declared wire alias, exact
canonical name/namespace, empty-object arguments, and unchanged call/result ID.
Signed cold writes and
compaction prefix reads must be nonzero. Checkpoint resume must not report an
old-prefix cache hit: it has no demonstrated shared cache-eligible prefix, so a
nonzero read on the first post-checkpoint inference fails acceptance rather than
being credited to preserved history. The later tool-result continuation may
legitimately reuse the new checkpoint prefix and is not required to have zero reads.
Exact successful request repeats are deliberate cache probes;
uncertain/failed attempts are never repeated. Tool code is never evaluated and
no external tool effect occurs. Actual signatures survive canonical JSON
serialization/deserialization; only the signed checkpoint replaces history at
the explicit compaction boundary.

`usage.jsonl` journals started/terminal outcomes and authoritative fresh/read/write/
output counts, raw upstream usage and stop reasons, without credentials or raw
request bodies. Failed receipts distinguish HTTP rejection, upstream error,
uncertain transport/timeout/truncation, and local validation. They retain bounded
HTTP status, upstream request ID, JSON `error.type`/`error.message`, and safe local
phase context. Error strings are credential-redacted; no header, URL, or error-body
dumps are retained. Missing/oversized/non-JSON error details are labeled omitted,
not reclassified as a successful request. Synthetic histories/checkpoints are retained in private files
(0700 directory, 0600 files on Unix). Preserve failed/unknown receipts. These are
transport, cache and signed-replay checks, **not** proof of core/UI integration, no-OpenAI
dispatch, real subagent dispatch, or account selection; those are separate gates.
Timing/measurement runs must use the machine's existing scheduler policy.
Normal completion/error removes the temporary auth directory. If the process is
killed, the operator must remove only the directory recorded in `setup.json`
after confirming this attempt has stopped; it contains a bearer-only credential.

Inputs and captures are limited to 16 cases and 256 KiB per line, and each
upstream body to 256 KiB.

## Subscription boundary

Live Sonnet 5 controls established that the fixed leading system block
`You are Claude Code, Anthropic's official CLI for Claude.` is required by the
subscription endpoint: adding only that block changed HTTP 429 to successful
inference; removing it restored the rejection. The successful minimal request
needed neither extra client-identification headers nor subscription metadata.
Native encoding adds that identity block and a fixed Codex-1337 harness block
before cache marking, without rewriting canonical history or the following
instructions. The harness identifies AGENTS.md as the project authority and
clarifies that Claude Code tools and commands are unavailable.

Apart from those fixed blocks, native encoding must not add prompt text, and it
must preserve the authority and exact content of system and developer instructions.
Tools need stable names and exact call/result pairing; `functions.exec` remains
Code Mode, `send_message` remains a direct family tool, and fixtures never
dispatch either. Root conversations mark cache breakpoints with a one-hour TTL
and subagents with the default five minutes; explicit TTLs, the
tools→system→history evaluation order and the four-breakpoint limit determine
cache behavior.

Upstream usage is authoritative. Input totals include uncached input, cache
writes and cache reads; charged compaction iterations are summed even when
top-level counters are zero. No character-based reasoning estimate participates
in the corpus's accounting contract.

## Evidence limits

The corpus covers fragmented text and tool arguments, Code Mode/custom and
parallel family calls, call/result continuation, namespace restoration, opaque
thinking/redacted payloads, signed compaction persistence, iteration accounting,
usage updates with omitted fields, ping, output limits, error and truncated EOF.

Warm cache repeats, appended results, persisted native resume, compaction
invalidation, TTL expiry, account isolation and real Haiku acceptance remain
integration gates. Synthetic counters establish accounting correctness, not a
cache-hit rate. A live comparison needs independent synthetic prefixes per arm
and separate upstream uncached/write/read/output measurements.
