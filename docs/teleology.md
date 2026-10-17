# Fork teleology

A telos is a purpose that a fork change serves and upstream does not; resolving a conflict the way upstream would can silently undo it. Each entry states one telos in six fields:

- Telos: why it exists.
- Holds: the invariant, stated so that a test can check it.
- Owners: the files and types that implement it.
- Tripwire: the test or check that fails when the invariant breaks. "Noisy" names broad snapshots that also change with unrelated upstream work and so tend to be accepted in bulk; "none" means no check fails.
- Pressure: how upstream threatens it. Pull request numbers name upstream changes between `rust-v0.160.0` and `rust-v0.162.0`.
- Evidence: fork commits.

Code marks a telos with `TELOS(slug): <one line>` in a doc or line comment where a merger decides: a seam in an upstream file, a fork-owned item whose bypass defeats the telos, or a fork default. Every tag names an entry here. Model-visible prompt templates never carry a tag, because it would reach the model. Text the fork deleted from the model context and capabilities it removed leave no code to tag; their entries name the absence check that guards them.

During a merge, look up each conflicted path in the index at the end and resolve the conflict so that every entry listed for it still holds. If upstream's change cannot coexist with an entry, ask the fork's maintainer instead of choosing.

## Model-context economy

### terse-exec-envelope

- Telos: a native execution result costs the model only what it acts on.
- Holds: rendered with `ExecutionOutputMetadata::Hidden`, an `exec_command` or `write_stdin` result is its output alone after exit 0, `Completed` after exit 0 without output, `Exit N` then output after another exit, and `Running (session S)` then output while running; it carries no wall time, chunk ID or token count, keeps truncation markers, and falls back to full JSON only for a contradictory status. MCP results carry no `Wall time` header.
- Owners: `codex-rs/protocol/src/native_execution_result.rs` (`NativeExecutionResult::render`, `render_batch`), `codex-rs/core/src/tools/context.rs` (`ExecCommandToolOutput::response_text`, `McpToolOutput`), `codex-rs/code-mode-runtime/src/runtime/native_result.rs`, `codex-rs/core/src/tools/code_mode/output.rs`.
- Tripwire: `native_result_presentation_follows_each_promise_and_current_values` (`codex-rs/code-mode-runtime/src/runtime/module_loader_tests.rs`); `exec_command_tool_output_*` in `codex-rs/core/src/tools/context_tests.rs`; `code_mode_can_return_exec_command_output` (`codex-rs/core/tests/suite/code_mode.rs`); the result parsers in `codex-rs/core/tests/suite/unified_exec.rs` and `approvals.rs`.
- Pressure: upstream tests assume the verbose envelope; 0.162 edits `unified_exec.rs` (3 commits), `approvals.rs` (4) and `code_mode.rs` (5) suites, including #50402 (`aggregated_output`).
- Evidence: 9ad927f511, 799b7a0998, 046e18a297.

### mcp-roster

- Telos: retain a broad tool catalogue without charging every conversation for every full schema. The short eager roster's per-tool context cost is intentional.
- Holds: in Code Mode Only, every nested tool appears by exact JavaScript member and one short purpose sentence, including tools whose contracts are deferred to search. Bootstrap `exec_command`, `apply_patch`, and `tool_search` keep eager porcelain contracts; MCP contracts, remaining builtin contracts, and MCP server instructions arrive through `describe_tool`. Non-MCP client-supplied dynamic tools retain their configured exposure policy. Provider-sensitive reference spelling never changes a JavaScript member or native wire alias. Disclosure never removes execution capability.
- Owners: `codex-rs/code-mode-protocol/src/roster.rs` (`UndeclaredTool`, `render_roster_for`), `codex-rs/code-mode-protocol/src/description.rs` (`build_exec_tool_description`), `codex-rs/core/src/tools/spec_plan.rs` (`register_code_mode_executors`), `codex-rs/core/src/tools/code_mode/disclosure.rs`, `codex-rs/core/src/tools/code_mode/execute_spec.rs`.
- Tripwire: absence of declarations: `roster_retains_every_callable_and_purpose_in_each_dialect` (`codex-rs/code-mode-protocol/src/roster_tests.rs`), `code_mode_uses_the_first_normalized_tool_identity` (`codex-rs/core/src/tools/spec_plan_tests.rs`), `code_mode_excludes_mcp_servers_using_their_configured_identity` (`codex-rs/core/tests/suite/code_mode.rs`), connector cases in `codex-rs/core/tests/suite/app_tool_exposure.rs`. Description token budget and server-instruction routing: none.
- Pressure: 0.162 rewrites the same builders: #51209 (ranked `tool_search` in Code Mode), #50562 (discovery guidance always present), #50536 (shared MCP types always present in Code Mode Only), #50687 (strict third-party deferral) edit `description.rs`, `spec_plan.rs` or `execute_spec.rs`.
- Evidence: 9b8fa789f6; owner clarification, 2026-10-13.

### append-only-tool-declarations

- Telos: a tool catalog change appends context instead of invalidating the conversation's cached prefix.
- Holds: `incremental_tools` defaults on; new Responses Lite context windows retain their initial declarations and append changed declarations and removal notices. The complete MCP name-and-purpose roster remains eager, including strict third-party mode; full MCP contracts remain on demand. Unsupported protocols retain their existing declaration path. An explicit `incremental_tools = false` selects that path for new windows; existing windows keep their established mode until replacement. Where declarations cannot be appended, cache churn from genuine catalogue or signature changes is accepted; this does not permit lookup-driven declaration changes.
- Owners: `codex-rs/features/src/lib.rs` (`Feature::IncrementalTools`), `codex-rs/core/src/session/step_context.rs` (`incremental_tools_enabled`), `codex-rs/core/src/session/world_state.rs` (`current_window_uses_incremental_tools`, `build_world_state_for_step`).
- Tripwire: `incremental_tools_append_changed_catalog_without_rewriting_history`, `tool_removal_appends_a_standalone_notice_without_rewriting_history` (`codex-rs/core/tests/suite/scenarios_incremental_tools.rs`), `strict_3p_mcp_refresh_preserves_cache_while_exec_tracks_available_tools` (`codex-rs/core/tests/suite/scenarios_strict_3p_cache.rs`), resume and opt-out cases in `codex-rs/core/tests/suite/scenarios_incremental_tools_resume.rs`.
- Pressure: upstream defaults this feature off; strict-third-party cache tests assume that no roster declaration changes, rather than permitting append-only updates.
- Evidence: release integration decision, 2026-10-11; owner clarification, 2026-10-13.

### compact-contracts

- Telos: discovery shows a compact summary; full contracts and output schemas cost tokens only when requested.
- Holds: `search_tools` and `ALL_TOOLS` give a name and one purpose sentence of at most 160 characters. `describe_tool` derives deterministic porcelain from the executable schemas: required and optional parameters, defaults, variants, constraints and return shape. References are expanded once per schema; input constraints are never silently budget-clipped. The obsolete Code Mode rendering-budget knob is removed; native MCP schema compaction is unchanged. `{detail: "output"}` discloses the complete declared result and `{detail: "schema"}` returns the retained machine schemas. MCP result fields are disclosed on demand, not repeated as a standing TypeScript preamble. For an unchanged catalogue, Code Mode discovery, contract lookup and first use never promote contracts into standing declarations or rewrite earlier context; their model-visible outputs append to history.
- Owners: `codex-rs/code-mode-protocol/src/discovery.rs` (`EnabledToolMetadata`, `compact_tool_summary`), `codex-rs/code-mode-protocol/src/contract.rs`, `codex-rs/code-mode-protocol/src/presentation.rs`, `codex-rs/code-mode-runtime/src/runtime/discovery.rs`, `codex-rs/core/src/tools/handlers/mcp.rs`, `codex-rs/core/src/tools/code_mode/execute_handler.rs`.
- Tripwire: `global_scope_contains_only_allowed_items` (`codex-rs/code-mode-runtime/src/service_tests.rs`) pins every discovery output; `contract_retains_shared_references_and_required_branches` (`codex-rs/code-mode-protocol/src/description.rs`); `mcp_code_mode_definitions_are_cached_lazily` (`codex-rs/core/src/tools/handlers/mcp.rs`) asserts the kept output schema. Request-level declaration stability across lookup and first use: unverified.
- Pressure: 0.162 #51209 returns TypeScript declarations from ranked `tool_search` and edits `execute_handler.rs`; #50687 changes MCP deferral in `handlers/mcp.rs`.
- Evidence: 3755ec0060, 87b76edda2, 8fc9bd12dc, 335de0047b; owner clarification, 2026-10-13.

### standing-context-trims

- Telos: the standing context carries nothing the model cannot use or already has.
- Holds (each an omission): `exec` documents `audio()` only for models with audio input and `generatedImage()` only when image generation is available; the three MCP resource tools are registered only when an initialized server advertises resources; `update_goal`'s description omits the completion and blocked audits that `ext/goal/templates/goals/continuation.md` carries; no model sees approved command prefixes; the permissions text omits writable roots and states its escalation paragraph once; the environment context folds rules repeated under every workspace root into `:workspace_roots` entries; mode texts state the current policy without retracting earlier ones; `exec_command` claims a PTY only for `tty`.
- Owners: `codex-rs/code-mode-protocol/src/description.rs` (`MediaHelpers`), `codex-rs/core/src/tools/spec_plan.rs`, `codex-rs/codex-mcp/src/connection_manager.rs` (`McpConnectionSet::offers_resources`), `codex-rs/codex-mcp/src/binding.rs`, `codex-rs/ext/goal/src/spec.rs`, `codex-rs/core/src/session/world_state.rs`, `codex-rs/prompts/src/permissions_instructions.rs`, `codex-rs/prompts/templates/permissions/approval_policy/on_request.md`, `codex-rs/core/src/context/environment_context/workspace_root_folding.rs`, `codex-rs/collaboration-mode-templates/templates/default.md`, `codex-rs/core/src/tools/handlers/shell_spec.rs`, `codex-rs/core/src/tools/handlers/unified_exec/exec_command.rs`.
- Tripwire: approved prefixes: `saved_prefix_only_bypasses_guardian_for_general_models` (`codex-rs/core/tests/suite/cyber_exec_policy.rs`). The rest is noisy: `codex-rs/core/tests/suite/snapshots/all__suite__scenarios__*.snap` and `codex-rs/core/src/context/world_state/snapshots/`; no absence or budget assertion.
- Pressure: 0.162 #50546 always registers the MCP resource tools in Code Mode Only so that tool definitions stay stable across server changes, reversing the resource gate; #50536 always includes the shared MCP type preamble; #50540, #50447 and #49560 edit `world_state.rs`; #50962 and #50741 edit `shell_spec.rs` and `exec_command.rs`.
- Evidence: e7942f28c6, 1ed0c355cd.

### claude-native-voice

- Telos: native Claude is addressed as itself inside Codex-1337, not as Claude Code or as a GPT.
- Holds: the second `system` block of every native request is `SUBSCRIPTION_HARNESS`; Claude catalog entries carry `ROOT_AGENT_ROLE` and `SUBAGENT_ROLE`, which use no GPT channel vocabulary, and list `request_user_input_async`; a namespace's description opens its first tool's description, and later members carry neither it nor a reference to it.
- Owners: `codex-rs/codex-api/src/anthropic/request.rs` (`SUBSCRIPTION_HARNESS`), `codex-rs/model-provider/src/claude_catalog.rs`, `codex-rs/core/src/client_anthropic_tools.rs` (`project_tools`).
- Tripwire: role texts: `account_routes_do_not_leak_into_catalog_or_admit_openai_fallbacks` (`codex-rs/model-provider/src/claude_catalog_tests.rs`). Harness block and namespace placement: none.
- Pressure: none in 0.162; every owner is fork-owned.
- Evidence: 9f19e7809e, 4d5c1e66f3.

### human-only-facts

- Telos: facts shown to the human cost no model tokens and leave the cached prefix unchanged.
- Holds: Code Mode ownership, MCP edit receipts, work status, the Preamble census, token bills, reasoning-token Thoughts and prompt-cache estimates travel as events or `TurnItem`s, never as `ResponseItem`s; `take_file_changes` strips receipts from the model-visible MCP result.
- Owners: `codex-rs/protocol/src/code_mode.rs` (`CodeModeOwner`, `CodeModeOwned`), `codex-rs/protocol/src/mcp_file_changes.rs` (`FileChangeReceipt`, `take_file_changes`), `codex-rs/core/src/mcp_tool_call.rs`, `codex-rs/core/src/tools/handlers/work_status.rs`, `codex-rs/protocol/src/preamble.rs` (`PreambleItem`), `codex-rs/protocol/src/items.rs` (`TurnItem`), `codex-rs/rollout/src/policy.rs`, `codex-rs/core/src/session/response_thought.rs`.
- Tripwire: receipts: `mcp_receipt_survives_event_bounds_without_entering_model_result` (`codex-rs/core/src/mcp_tool_call_tests.rs`), `receipt_capture_is_private_and_rejection_does_not_change_tool_failure` (`codex-rs/protocol/src/mcp_file_changes_tests.rs`). Other facts: none; only the type split guards them.
- Pressure: 0.162 #50458 (truncated MCP results in paginated history) and #50435 (tool definitions in rollout history) edit `mcp_tool_call.rs`, `rollout/src/policy.rs` and `protocol/src/models.rs`.
- Evidence: 67c459e3df, 900acaab71, dba3834511, 40394ab352.

### bare-model-slugs

- Telos: agent model selectors cost and read as bare slugs.
- Holds: `spawn_agent` advertises every selectable, backend-supported model by its bare slug unless that slug occurs in more than one provider's multi-agent catalog, hidden entries included, and then as `provider/slug`. Catalog budgets may shorten prose, never omit selectors or capabilities.
- Owners: `codex-rs/core/src/agent/model_catalog.rs` (`advertised_agent_models`).
- Tripwire: `fresh_child_routes_model_and_provider_but_history_forks_stay_locked` (`codex-rs/core/src/agent/model_catalog_tests.rs`).
- Pressure: 0.162 #49560 adds an opt-in model catalog to the multi-agent context through `spec_plan.rs` and `world_state.rs`.
- Evidence: 8b99bb1b59.

### turn-economy

- Telos: the model spends a round trip only when there is news.
- Holds: Code Mode `exec` and `wait` return only when the cell finishes or yields, user input or released mail arrives, or `[features.code_mode] max_yield_time_ms` (default 600000) expires, and ignore the model's `yield_time_ms`; in Code Mode Only, `exec_command` waits for exit unless `background` is set, and an empty `write_stdin` from a cell reports a running process no sooner than 60 s; ordinary family mail wakes idle recipients and batches for one fixed window while at least two subagents are active anywhere in the family; `wait_agent` observes released mail without adding another hold; Code Mode exposes `clock.sleep`.
- Owners: `codex-rs/core/src/session/input_queue/interjection.rs` (`Interjection`), `codex-rs/core/src/tools/code_mode/wait_handler.rs`, `codex-rs/core/src/tools/code_mode/execute_handler.rs`, `codex-rs/core/src/session/input_queue/mailbox_batch.rs`, `codex-rs/core/src/unified_exec/initial_wait.rs` (`InitialWait`), `codex-rs/core/src/tools/handlers/unified_exec/exec_command/cell.rs`, `codex-rs/core/src/tools/handlers/unified_exec/write_stdin/cell.rs` (`EMPTY_WRITE_FLOOR_MS`), `codex-rs/core/src/tools/spec_plan.rs`.
- Tripwire: `interjections_yield_exec_and_wait_without_stopping_the_cell` and `wait_agent_wake_preserves_mail_and_steers` (`codex-rs/core/tests/suite/pending_input.rs`), `code_mode_can_return_exec_command_output` (`codex-rs/core/tests/suite/code_mode.rs`), `sleep_tool_uses_configured_time_provider` (`codex-rs/core/tests/suite/current_time_reminder.rs`). The 60 s floor: none.
- Pressure: 0.162 #51812 enables `instant_interrupt` by default, which decides whether an `Interjection` cuts off streaming, and edits `pending_input.rs`; #51209 edits `code_mode/mod.rs` and `execute_handler.rs`; #51332 edits `multi_agents_v2/wait.rs`; #50962 and #50741 edit `exec_command.rs` and `write_stdin.rs`.
- Evidence: 25226df915, 2d9f646044, 9050de4515, cc4701d2fe.

### local-coprocessor

- Telos: local computation and bounded file transfers need no shell quoting or extra model turn.
- Holds: raw Python and Bash source executes through the selected executor with ordinary execution policy, approval and cancellation; script execution does not acquire shell startup, snapshot or command-prefix approval semantics. Code Mode filesystem helpers use the captured step's environment and granted permissions, never the host's ambient filesystem authority; each transfer is bounded to 1 MiB.
- Owners: `codex-rs/core/src/tools/script_exec.rs` (`ExecutionMode`), `codex-rs/core/src/tools/handlers/script_exec/`, `codex-rs/core/src/exec_policy/script_exec.rs`, `codex-rs/core/src/tools/handlers/code_mode_fs.rs`, `codex-rs/code-mode-runtime/src/runtime/filesystem.rs`; seams in `tools/spec_plan.rs`, `tools/handlers/unified_exec/exec_command.rs`, `tools/runtimes/unified_exec.rs` and `unified_exec/process_manager.rs` under `codex-rs/core/src/`.
- Tripwire: `raw_script_execution_contract` (`codex-rs/core/tests/suite/raw_script_exec.rs`), `filesystem_authority_and_transfer_bounds` (`codex-rs/core/tests/suite/code_mode_pipeline.rs`).
- Pressure: changes to shell invocation, snapshot preparation, executor selection or turn permission grants must preserve script-versus-shell authority; Code Mode description overrides must not hide the filesystem helpers.
- Evidence: 0dc126edd9, 0a51b60ef1.

### response-order-fences

- Telos: dependent execution stages fit in one model response without racing or running after a failed prerequisite.
- Holds: calls enter their response batch in stream order; a fence admits the next stage only after every preceding call has completed successfully. Cancellation, dropped calls, yielded cells, background processes and nested failures fail closed; catching a nested error does not erase it. Admission waits occur outside execution locks, and Code Mode coordinators hold no lock while their nested tools execute. A new model response starts a new batch.
- Owners: `codex-rs/core/src/tools/fence.rs`, `codex-rs/core/src/tools/parallel.rs`, `codex-rs/core/src/stream_events_utils.rs`, `codex-rs/core/src/session/turn.rs`, `codex-rs/core/src/tools/code_mode/delegate.rs`, `codex-rs/core/src/tools/spec_plan.rs`, `codex-rs/code-mode-protocol/src/description.rs`.
- Tripwire: `response_stage_pipeline` (`codex-rs/core/tests/suite/code_mode_pipeline.rs`) covers successful and failed stages; scheduler and cancellation cases live beside the fence implementation.
- Pressure: stream dispatch, parallel-tool locking and Code Mode worker boundaries can lose ordering or nested failure attribution; model description overrides must retain the fence contract.
- Evidence: 7e44726e4e.

### retained-goal-rules

- Telos: continuing a goal pays for its full rules once per retained context, rather than once per continuation.
- Holds: the goal extension contributes full continuation rules as an owned world-state section and emits short subsequent reminders. A saved section snapshot cannot suppress rules absent from retained history; only a complete, kind-aligned fragment with matching provenance proves presence, never unannotated matching user text.
- Owners: `codex-rs/ext/goal/src/world_state.rs`, `codex-rs/ext/goal/src/extension.rs`, `codex-rs/ext/goal/src/runtime.rs`, `codex-rs/core/src/context/world_state/mod.rs`; prompt text remains in `codex-rs/ext/goal/templates/goals/`.
- Tripwire: `missing_retained_fragment_is_rendered_again` (`codex-rs/core/src/context/world_state/world_state_tests.rs`); existing goal continuation cases cover the emitted reminders.
- Pressure: compaction, history truncation and changes to world-state rendering or content-kind alignment must not drop rules or restore repeated full instructions.
- Evidence: a7c3becfcf.

## Prompt-cache stability

### instruction-amendments

- Telos: ordinary instruction changes append amendments rather than rewrite the Preamble.
- Holds: managed instructions retain their original authority and use literal unified diffs. The harness keeps canonical full values and requires complete, kind-aligned retained ancestry before suppressing a reinjection. Compaction receives a disposable view with current full instructions substituted for their older versions and amendments; conversation evidence and opaque provider checkpoints remain intact. Only successful compaction replaces live history and starts a new window. Model/capability changes, local tip repairs and unsupported incremental tool protocols retain their existing behavior; rollback has no special optimization.
- Owners: `codex-rs/core/src/context/world_state/instruction_diff.rs`, `codex-rs/core/src/context/world_state/instruction_projection.rs`, `codex-rs/core/src/context_manager/instruction_projection.rs`, `codex-rs/core/src/context/base_instructions.rs`.
- Tripwire: `render_diff_restores_the_typed_section_snapshot`, `catalog_collaboration_messages_refresh_without_mode_or_model_change`, `compaction_preserves_updated_summary`.
- Pressure: instruction providers must expose a canonical full value without emission side effects; fragment provenance and authority must survive replay. Skills opt in through `WorldStateSectionContribution::with_instruction_amendments`: added/changed/removed summaries use unaliased paths and share the harness's retention and compaction machinery. Bounded catalogs relist rather than amend incomplete membership. Legacy revocations without a canonical predecessor retain explicit notices outside materialization. `production_turn_keeps_host_aliases_and_amends_catalog_changes` covers amendment materialization and cold replay.

### reminders-not-system

- Telos: an instruction update never rewrites an earlier cached prefix.
- Holds: besides the two subscription blocks, a native request's `system` holds only instructions that precede the first message; each later system or developer item becomes, at its original position, a user message of `<system-reminder>` text blocks.
- Owners: `codex-rs/codex-api/src/anthropic/history.rs` (`Encoder`).
- Tripwire: `positional_instruction_updates_do_not_rewrite_earlier_prefixes` (`codex-rs/codex-api/src/anthropic/history_tests.rs`).
- Pressure: 0.162 #51156 moves base instructions from the `instructions` field into a leading `developer` input message for Responses requests; the native encoder reads `prompt.instructions`.
- Evidence: 15bb22a28e, e2e1f4a101.

### catalog-at-model-boundaries

- Telos: a changing MCP catalog never alters the declarations of a request in flight.
- Holds: an ordinary MCP list-changed notification refreshes catalogs and invalidates cached bindings only at the next model boundary; a call whose advertised declaration differs from the live one is rejected, except that a listing whose annotations a cache or dormant server withheld (`AnnotationSource::Withheld`) dispatches with the live annotations; an annotation change between two live listings still rejects.
- Owners: `codex-rs/codex-mcp/src/connection_manager/tool_catalog.rs` (`prepare_call_for_tool`), `codex-rs/codex-mcp/src/client_tool_catalog.rs`, `codex-rs/codex-mcp/src/runtime.rs`, `codex-rs/codex-mcp/src/tools.rs` (`AnnotationSource`), `codex-rs/codex-mcp/src/tool_catalog_cache.rs`, `codex-rs/core/src/mcp_tool_call.rs`.
- Tripwire: `ordinary_notifications_invalidate_cached_bindings_at_the_next_boundary` (`codex-rs/codex-mcp/src/runtime/catalog_refresh_tests.rs`), `ordinary_catalog_refresh_coalesces_and_preserves_failed_and_racing_notifications` (`codex-rs/codex-mcp/src/connection_manager/tool_catalog_refresh_tests.rs`), which covers annotation-only edits. Withheld-annotation dispatch: none.
- Pressure: 0.162 incremental tool catalog updates (#50464, #50540, #51119, #51188, #51202, #51480) change the declared catalog between requests within a context window; #51215 edits `tool_catalog.rs`.
- Evidence: e2267aec40, d1efade548, 32ac1fd013.

### stable-native-identities

- Telos: catalogue changes, retirements and resumes leave a native conversation's bytes, and so its cache prefix, unchanged.
- Holds: a native tool's wire alias depends only on its identity (`AnthropicToolIdentity::wire_alias`), never on catalogue order or membership; history naming a retired tool replays identically without re-declaring it; a failed tool output resumes with `success == Some(false)` through the persisted `tool_failed` harness mark.
- Owners: `codex-rs/codex-api/src/anthropic/tool_alias.rs`, `codex-rs/core/src/client_anthropic_tools.rs`, `codex-rs/codex-api/src/anthropic/history.rs`, `codex-rs/history/src/tool_failure.rs`.
- Tripwire: `aliases_preserve_dispatch_identity_across_collisions_and_catalogue_order` (`codex-rs/core/src/client_anthropic_tools_tests.rs`), `signed_history_and_parallel_tool_results_preserve_cache_prefix` (`codex-rs/codex-api/src/anthropic/history_tests.rs`), `resumed_native_history_replays_tool_failure` (`codex-rs/core/src/session/rollout_reconstruction_tests.rs`), `native_request_pins_recovery_then_switches_with_exact_custom_tool_replay_and_cache_warm` (`codex-rs/core/src/client_anthropic_tests.rs`).
- Pressure: 0.162 #50435 persists additional tool definitions in rollout history and #49401 keeps live tool-call metadata across request windows, both in `protocol/src/models.rs`.
- Evidence: ea7dfec93c, 60dd4c7118, ef357510f1, 846fd2257f.

### cache-ttl-and-warming

- Telos: cache warmth follows observed usage; hour-lived Claude caches are chosen where they pay and kept warm while the human is present.
- Holds: this deployment uses GPT-6-family or newer OpenAI models. Their conversation requests establish a 30-minute passive Warm estimate only after reporting cache reads or writes; API and subscription routes use the same window. Other Responses providers do not inherit that policy. Expiry runs conservatively from dispatch; prefix invalidation and absent cache usage clear the estimate. A restart creates no lease from thread metadata. This does not enable OpenAI `/warm` or send background requests.
- Holds: a native request's cache TTL is that of the first `anthropic_prompt_cache.rules` entry matching its model prefix and effective effort, else `ttl` (default one hour); `/warm` toggles warming only for hour-lived root Claude caches; a hold lasts 8 h from the last human message or toggle; the sealed request replays 3 min before expiry; any lapse turns warming off; warming never heats a cold cache.
- Owners: `codex-rs/core/src/client_prompt_cache.rs`, `codex-rs/config/src/anthropic_prompt_cache.rs` (`AnthropicPromptCache::resolve`), `codex-rs/codex-api/src/anthropic/cache.rs`, `codex-rs/codex-api/src/anthropic/warm.rs`, `codex-rs/app-server/src/prompt_cache.rs`, `codex-rs/app-server/src/request_processors/thread_warming.rs`, `codex-rs/app-server-protocol/src/protocol/v2/prompt_cache.rs`.
- Tripwire: `ordered_rules_filter_effective_effort_and_validate_selectors` (`codex-rs/config/src/anthropic_prompt_cache_tests.rs`), `observed_cache_usage_controls_warmth_across_completion_attach_and_restart` (`codex-rs/app-server/tests/suite/v2/thread_warming.rs`), `warming_and_cold_glyphs_share_the_bottom_padding_row` (`codex-rs/tui/src/bottom_pane/chat_composer/prompt_cache_glyphs_tests.rs`), the TTL markers in `native_request_pins_recovery_then_switches_with_exact_custom_tool_replay_and_cache_warm`.
- Pressure: none in 0.162; every owner is fork-owned.
- Evidence: fc408f929c, 920a743485.

## Token visibility

### token-bills

- Telos: the human sees what consumes the context.
- Holds: every tool output, MCP result and Code Mode result disclosure shows its approximate model-visible tokens, after truncation for native Claude; each completed model response yields a human-only Thought with its reported reasoning tokens; bills render in the fixed rich-transcript gutter. Preamble records a window's initial special material once, preserving typed prompt captures separately from client declaration representations, and survives reconnect and restart without a new census. Later changes appear as Injections; `/context` inspects retained special material without injecting it. Legacy rolling censuses remain readable under their own label. Census work never blocks response-usage recording or cancellation cleanup.
- Owners: `codex-rs/core/src/session/tool_output_tokens.rs`, `codex-rs/utils/output-truncation/src/token_count.rs`, `codex-rs/core/src/session/response_thought.rs`, `codex-rs/core/src/session/preamble.rs`, `codex-rs/app-server-protocol/src/protocol/v2/thought.rs`, `codex-rs/protocol/src/preamble.rs`, `codex-rs/tui/src/transcript_view/text_gutter.rs`, `codex-rs/tui/src/history_cell/preamble.rs`.
- Tripwire: `tool_text_matches_reference_tokenizer_without_interpreting_control_markers` (`codex-rs/utils/output-truncation/src/token_count_tests.rs`), `thought_completion_is_self_contained_and_response_scoped` (`codex-rs/app-server-protocol/src/protocol/thread_history_projection_tests.rs`), `structured_content_remains_disclosable_in_direct_and_code_mode_results` (`codex-rs/tui/src/history_cell/mcp_tests.rs`), `tool_output_disclosure_shows_fixed_token_column` (`codex-rs/tui/src/transcript_view/layout_tests.rs`), `preamble_discloses_parts_largest_first` (`codex-rs/tui/src/history_cell/preamble_tests.rs`).
- Pressure: none in 0.162; every owner is fork-owned.
- Evidence: 5b6e3b5096, 0a2e39ff0f, 86f8dba3aa, cabee7f17e, 2213557e47, 40394ab352.

## Native Claude as a first-class provider

### messages-from-canonical-history

- Telos: native Claude reads Codex's canonical history directly, and no history it cannot encode wedges a thread.
- Holds: Messages requests are encoded item by item from canonical `ResponseItem` history, not through a Responses emulation; content recorded during an unresolved tool batch follows the batch's results; `salvage_anthropic_history` always yields an encodable history and reports each `AnthropicRepair`; native compaction descends the `Rung` ladder (whole history, tail, recent user messages), and when even the last rung returns no checkpoint the thread resets to that material without a model call; each concession is a warning; a refused sampling history compacts once per step and retries.
- Owners: `codex-rs/codex-api/src/anthropic/history.rs`, `codex-rs/codex-api/src/anthropic/salvage.rs`, `codex-rs/core/src/compact_native.rs` (`Rung`), `codex-rs/core/src/compact_native_request.rs`.
- Tripwire: `content_recorded_during_a_tool_call_follows_its_batch` and `salvage_always_yields_an_encodable_history` (`codex-rs/codex-api/src/anthropic/history_tests.rs`), `unusable_completions_record_usage_and_name_their_rung_escalation` (`codex-rs/core/src/compact_native_request_tests.rs`).
- Pressure: 0.162 #51156 moves base instructions into input messages; #51117 installs full context in compaction replacement history (`compact.rs`, `compact_remote_v2.rs`); #51188 records base instructions in incremental tool history.
- Evidence: 65b19edd6c, d68f5c1e02.

### subscription-boundary

- Telos: the fork uses a Claude subscription only as Anthropic's own client would, and never leaks its credentials.
- Holds: the first `system` block of every native request is exactly `SUBSCRIPTION_PREAMBLE`; a `ClaudeSubscription` provider sends credentials only to `https://api.anthropic.com/v1` and admits no custom credentials, headers or query parameters; a request captures its account before any await, and refresh and dispatch keep that account when the selection changes.
- Owners: `codex-rs/codex-api/src/anthropic/request.rs` (`SUBSCRIPTION_PREAMBLE`), `codex-rs/model-provider-info/src/lib.rs` (`ANTHROPIC_BASE_URL`, `ModelProviderInfo::validate`), `codex-rs/core/src/client_anthropic.rs` (`capture_anthropic_auth`, `with_anthropic_account`).
- Tripwire: account capture: `native_request_pins_recovery_then_switches_with_exact_custom_tool_replay_and_cache_warm` (`codex-rs/core/src/client_anthropic_tests.rs`). Preamble and endpoint: none; `scripts/native_anthropic/check.py` is manual, and its corpus predates the harness block.
- Pressure: 0.162 #50459 adds capability overrides for custom providers in `model-provider-info/src/lib.rs`.
- Evidence: 60e8198df4, b674b02d5a, 4157a2e4ea, ea648c2824.

### native-robustness

- Telos: a provider anomaly costs at most the affected call, and every charged token is recorded.
- Holds: a `tool_use` that names an undeclared tool or whose input does not fit its tool's envelope becomes `ResponseItem::RejectedToolCall` (`ToolCallRejection`): it is answered, not dispatched, and later blocks and usage survive; charged usage is recorded before an exhausted or refused response is rejected; subscription exhaustion maps to a usage-limit error; a refusal ends the turn with a terminal error.
- Owners: `codex-rs/codex-api/src/anthropic/decoder.rs` (`AnthropicStopReason`), `codex-rs/protocol/src/models.rs` (`ToolCallRejection`), `codex-rs/core/src/stream_events_utils.rs`, `codex-rs/codex-api/src/anthropic/limits.rs` (`parse_usage_limit`), `codex-rs/core/src/session/turn.rs`, `codex-rs/core/src/compact_native_request.rs`.
- Tripwire: `undeclared_tool_preserves_stream_and_replays_as_a_paired_rejection`, `malformed_declared_tool_preserves_later_blocks_and_usage` and `interleaved_tools_signed_blocks_and_cumulative_usage` (`codex-rs/codex-api/src/anthropic/decoder_tests.rs`), `rejected_native_call_is_not_dispatched_and_keeps_its_result_on_resume` (`codex-rs/core/src/stream_events_utils_tests.rs`), `map_api_error_distinguishes_anthropic_subscription_exhaustion` (`codex-rs/codex-api/src/api_bridge_tests.rs`). Usage before rejection in the sampling path: none.
- Pressure: 0.162 #51241 and #51249 add a partial-answer message phase in `protocol/src/models.rs` and `stream_events_utils.rs`; `session/turn.rs` takes 16 upstream commits.
- Evidence: df7be75a07, f21241bd52, 6fa07b94ea, e12016f164, d54b293a72.

### adapter-not-transport

- Telos: provider semantics are a provider property, independent of the Responses wire format it may share.
- Holds: `ModelProviderAdapter`, not `WireApi`, selects provider semantics; context-window and compaction defaults follow the selected provider without being written into config; each provider ID has its own models cache file (`models_cache.json` for OpenAI, `models_cache_<id>.json` otherwise).
- Owners: `codex-rs/model-provider-info/src/lib.rs` (`ModelProviderAdapter`, `models_cache_file_name`), `codex-rs/core/src/config/models_cache.rs` (`Config::models_cache_path`), `codex-rs/core/src/config/mod.rs`, `codex-rs/core/src/session/context_window.rs`.
- Tripwire: `provider_context_defaults_follow_selection_without_baking_into_config` (`codex-rs/core/src/config/config_tests.rs`). Per-provider cache file: none.
- Pressure: 0.162 #50459 adds capability overrides for custom providers in `model-provider-info/src/lib.rs`; `core/src/config/mod.rs` takes 12 upstream commits.
- Evidence: 903087f7a4, f48c034081, 45892d83cc.

### provider-parity

- Telos: OpenAI and Anthropic are equally trusted, so either may serve any role.
- Holds: a privacy or data-locality rule permits or forbids both providers alike; a Claude conversation's approval review runs on an eligible saved ChatGPT account's auto-review model, and an individual review falls back to Claude for good after its first failed OpenAI attempt, within the shared deadline.
- Owners: `docs/fork.md` (Provider data), `codex-rs/core/src/guardian/reviewer_route.rs`.
- Tripwire: `claude_subscription_review_spawns_chatgpt_reviewer_with_saved_account` and `route_failure_quarantines_and_falls_back_only_with_remaining_time` (`codex-rs/core/src/guardian/reviewer_route_tests.rs`). The data rule: none.
- Pressure: 0.162 #51137 recovers Guardian reviews from parent checkpoints and #51223 edits `guardian/review_session.rs`.
- Evidence: ad002e7548, ff65685666.

## Tool authority

### managed-process-visibility

- Telos: the managed local server permits the owner's chosen host-process and user-service access without changing every execution request.
- Holds: production and staging units explicitly select inherited Linux PID/proc visibility; ordinary app-server startup still defaults to isolation. The trusted startup choice reaches only the local executor, never remote executor policy or a repository/model permission override. Other directly executed sandbox restrictions remain unchanged. Same-UID signaling and host-user-manager escape from tool containment are intentional operator authority; see `docs/fork.md` Local TUI releases.
- Owners: `codex-rs/app-server/src/local_exec.rs`, `codex-rs/sandboxing/src/linux_pid_namespace.rs`, `scripts/systemd/codex-app-server.service`, `scripts/systemd/codex-app-server-staging.service`.
- Tripwire: existing app-server startup/config and installer-unit checks cover argument propagation; host D-Bus behavior has a recorded read-only A/B witness, not a portable unit-test claim.
- Pressure: upstream sandbox defaults or daemon argument rebuilding must not erase the explicit managed-unit policy or silently apply it to standalone/remote executors.
- Evidence: 649efc61dc.

### startup-tool-ceiling

- Telos: an ephemeral utility model receives data without acquiring tool authority through configuration or provider changes.
- Holds: the captured startup tool policy is immutable and can only narrow inherited authority. `noTools` permits no direct or nested dispatch and starts no MCP contributors, including required servers. The frontend sends utility transcript/inference only after a receipt derived from the captured policy confirms the restriction; absent receipts fail closed. This nonpersisted startup restriction requires an ephemeral thread.
- Owners: `codex-rs/protocol/src/tool_policy.rs`, `codex-rs/core/src/tool_policy.rs`, `codex-rs/core/src/tools/registry.rs`, `codex-rs/tui/src/temporary_structured_request.rs`.
- Tripwire: `startup_allowlist_controls_advertising_and_execution` (`codex-rs/core/tests/suite/managed_threads_tests.rs`), `dispatch_uses_canonical_tool_names_for_lifecycle_contributors` (`codex-rs/core/src/tools/registry_tests.rs`), and the existing recap/older-server interactions.
- Pressure: startup, extension refresh and discovery changes must preserve the ceiling at advertising and execution, not merely hide schemas or maintain a feature denylist.
- Evidence: 7b8ddaa991.

### canonical-tool-identity

- Telos: formatting a tool name cannot broaden a hook, change attribution, or collide with another tool.
- Holds: namespace and local name remain one structured `ToolName` through dispatch, hooks and delayed execution receipts. `HookSelector` distinguishes tool identity from lifecycle-event values; unknown fields and selectors invalid for an event fail closed. Provider aliases and display strings are boundary projections, never policy identities; obsolete flattened matchers have no compatibility fallback.
- Owners: `codex-rs/protocol/src/tool_name.rs`, `codex-rs/protocol/src/hook_selector.rs`, `codex-rs/hooks/src/engine/selection.rs`, `codex-rs/core/src/tool_policy.rs`.
- Tripwire: existing hook selection/configuration interactions in `codex-rs/hooks/src/engine/mod_tests.rs` and `codex-rs/hooks/src/config_rules.rs`; original-tool attribution through execution is covered by the core hook integration cases.
- Pressure: upstream changes to tool naming, delayed process receipts or hook configuration must not reintroduce concatenated identities or turn an invalid selector into an unfiltered hook.
- Evidence: 46833d37fa, 731f798e13, 09cb78a61c.

## Server-owned coordination

### accepted-agent-mail

- Telos: ordinary mail acknowledged as accepted survives a server crash before model admission without becoming duplicate or revived work.
- Holds: the authoritative inbox commits a stable identity, attribution and replay digest before acceptance. Finite owned admission drains on shutdown and commits canonical history before acknowledging the row; an uncertain canonical write fences new inference until canonical recovery. Lookup checks retained raw lineage, past compaction but within explicit cutoffs, and never treats an old admission witness as current context membership. Revert acknowledges already-canonical pending rows before discarding their witnesses; genuinely unadmitted rows remain pending. Recovery requires no frontend, retains blocked mail and uses bounded in-memory backoff. The envelope is process crash with intact storage, not a preacceptance completion outbox.
- Owners: `codex-rs/core/src/agent/control/accepted_mail.rs`, `codex-rs/core/src/session/agent_mail.rs`, `codex-rs/state/src/runtime/agent_mail.rs`, `codex-rs/thread-store/src/live_thread/agent_mail.rs`, `codex-rs/thread-store/src/local/agent_mail.rs`, `codex-rs/app-server/src/request_processors/agent_mail.rs`.
- Tripwire: `record_inter_agent_communication_preserves_item_id_in_rollout_and_resume` (`codex-rs/core/src/session/tests.rs`), `revert_keeps_thread_id_and_hides_suffix_across_repeated_reverts` (`codex-rs/thread-store/src/local/revert_thread_tests.rs`), and the existing idle-mail/successor-process recovery interactions.
- Pressure: queued-input, task registration, compaction, rollback, lineage and shutdown changes must preserve acceptance versus admission and canonical uncertainty; neither latest-model-context reads nor best-effort channel sends replace the durable contract.
- Evidence: 53d5d972e3, c63e481a76, 2a47c1fcc3, 519609ba40, c882db1102.

### shared-daemon

- Telos: one shared app server owns all conversations, and installing a release never interrupts work.
- Holds: the TUI refuses an embedded app server unless `CODEX_ALLOW_EMBEDDED_SERVER` is set; SIGHUP, which `codex-app-server-refresh.service` sends when the installed release changes, defers replacement until running turns and admission permits are idle while resumes and new work are still admitted; explicit shutdown stays forceable; systemd kills surviving tool descendants once the daemon exits (`KillMode=mixed`).
- Owners: `codex-rs/tui/src/lib.rs`, `codex-rs/app-server/src/shutdown.rs` (`ShutdownState`), `codex-rs/app-server/src/turn_admission.rs`, `scripts/systemd/codex-app-server.service`, `scripts/systemd/codex-app-server-refresh.path`, `scripts/systemd/codex-app-server-refresh.service`.
- Tripwire: `shutdown_waits_for_admitted_requests_but_force_remains_available` and `pending_upgrade_keeps_admission_open_until_requests_and_turns_are_idle` (`codex-rs/app-server/src/shutdown_tests.rs`), `websocket_transport_pending_upgrade_allows_resume_and_new_turns_until_idle` (`codex-rs/app-server/tests/suite/v2/connection_handling_websocket_unix.rs`). The embedded-server refusal (compiled out of tests) and the unit files: none.
- Pressure: 0.162 #50803 (managed daemon for remote-control launches) and #50913 edit `tui/src/lib.rs`, which takes 9 upstream commits; #51470 edits `app-server/src/lib.rs`.
- Evidence: 2df4543ce0, 5aa88dcd33, 43a0125c85.

### reload-host

- Telos: the TUI process can be replaced without losing input or its server connection.
- Holds: `/reload` and idle restart hand `RetainedInput` to the new UI generation without detaching the server connection; a candidate is admitted only after its probe reports a checkpoint reader that reads the current checkpoint; handoff and rollback share one deadline; a rejected or stalled candidate never consumes launch input.
- Owners: `codex-rs/tui/src/reload/mod.rs`, `codex-rs/tui/src/reload/host.rs`, `codex-rs/tui/src/reload/retained.rs` (`RetainedInput`), `codex-rs/tui/src/reload/admission.rs`, `codex-rs/tui/src/reload/deadline.rs`, `codex-rs/tui/src/app/ui_reload.rs`, `codex-rs/tui/src/app/idle_restart.rs`, `codex-rs/cli/src/reload_probe.rs`.
- Tripwire: `live_reload_retains_steering_and_times_out_without_replaying_input` (`codex-rs/tui/tests/suite/ui_reload.rs`), `stalled_and_rejected_workers_share_the_original_deadline` (`codex-rs/tui/src/reload/deadline_tests.rs`), `incompatible_or_stalled_probe_never_runs_launch_input` (`codex-rs/tui/src/reload/worker_process_tests.rs`), the unit tests in `codex-rs/tui/src/reload/control.rs`.
- Pressure: owners are fork-owned; the seams in `cli/src/main.rs` (#50803, #50437, #49939) and `tui/src/lib.rs` change in 0.162.
- Evidence: 183bbf0d06, fac940a210, ab654c8333.

### server-hosted-agents

- Telos: agent coordination lives in the app server, so it survives frontends and restarts.
- Holds: task tools run in the app server, not the TUI; deliveries to a busy thread queue durably with attribution and never become user input; subagent families survive restarts, and only an idle subtree can be retired; farm requests persist; an active root thread's canonical name is unique; each thread's comms gate and context role are server state. A farm reservation is keyed by request identity before path allocation; only proven rejection releases the reserved path. Allocated or uncertain outcomes retain custody and are not silently retried. Cross-provider automatic farming defaults to Fresh independently of per-provider executor configuration.
- Owners: `codex-rs/app-server/src/thread_tools/mod.rs`, `codex-rs/app-server-protocol/src/delegation.rs`, `codex-rs/app-server/src/request_processors/thread_queue_processor.rs`, `codex-rs/ext/queue/src/service.rs`, `codex-rs/core/src/agent/control.rs`, `codex-rs/core/src/agent/control/retirement.rs`, `codex-rs/core/src/tools/handlers/multi_agents_v2/retire_agent.rs`, `codex-rs/state/src/runtime/farm_requests.rs`, `codex-rs/state/src/runtime/thread_names.rs`, `codex-rs/app-server/src/request_processors/comms_gate.rs`, `codex-rs/protocol/src/comms_gate.rs`, `codex-rs/app-server/src/request_processors/context_role.rs`, `codex-rs/protocol/src/context_role.rs`. Reservation authority: `codex-rs/core/src/agent/control/farm_reservation.rs`.
- Tripwire: `codex-rs/app-server/src/thread_coordination_lifecycle_tests.rs`, `codex-rs/app-server/tests/suite/v2/server_coordination_*tests.rs`, `queued_tool_output_waits_for_completion_without_becoming_user_input` (`codex-rs/app-server/tests/suite/v2/thread_queue.rs`), `v2_family_survives_unload_until_idle_subtree_retirement` (`codex-rs/core/src/agent/control_tests.rs`), `farm_spawns_and_replays_without_steering_parent` (`codex-rs/app-server/tests/suite/v2/thread_farm.rs`), `root_name_claim_race_and_archive_lifecycle` (`codex-rs/state/src/runtime/thread_names_tests.rs`), `closed_idle_destination_bounces_messages_until_reopened` and `context_role_is_discoverable_until_cleared`.
- Pressure: 0.162 edits `core/src/agent/control.rs` with #51415 (turn lineage), #51329 (no partial-history subagent forks), #50087 (queued agent mail across eviction), #49814 and #51515 (coordinated agent-tree shutdown); #51415 also edits `thread_queue_processor.rs`. Path allocation and replay must not mistake an uncertain accepted request for a fresh reservation.
- Evidence: fa1cf4ce29, 8e8bb48075, c8dec1e3eb, 217bcce455, db62274156, eb8132ec04, f4588c72c6, e373b1520b, e331fec3c1, 2d9abac7cf.

### durable-approvals-and-queues

- Telos: a reconnect loses no pending decision or queued edit.
- Holds: an unresolved approval request survives transcript eviction and replays after buffered events; reconnect restores pending edits as drafts without submitting them. The UI's operation ledger owns dispatch, thread-or-connection origin, captured store/epoch, ordered configuration barriers and completion. Unsent or definitively rejected drafts are recoverable; unknown effects are never replayed or reported as success. Urgent interrupts, response delivery and exit control bypass ordinary submissions. Navigation retains resource-producing requests after cancellation and cleans known uninstalled subscriptions through the original connection. Only confirmed detachment changes attachment state and recovers unsent work. Accepted modal consent retains its exact write and continuation behind the configuration barrier; supersession cannot cancel the write. Directory replacement revalidates the source, input and effective permissions, then detaches the captured source before its remaining graph. Permission confirmation does not revoke unchanged authority. Ordinary forks retain source-family input and approvals; safety retries require explicit authorization, fork before the interrupted turn and preserve its input and permissions.
- Owners: `codex-rs/tui/src/app/pending_interactive_replay.rs`, `codex-rs/tui/src/app/thread_event_buffer.rs`, `codex-rs/tui/src/app/queued_messages.rs` (`QueuedMessages`), `codex-rs/tui/src/app/reconnect.rs`. `codex-rs/tui/src/app/turn_submissions.rs` (`ServerOperations`), `codex-rs/tui/src/app/turn_submission_controller.rs`, `codex-rs/tui/src/app/thread_routing.rs`, `codex-rs/tui/src/app/server_operation_requests.rs`, `codex-rs/tui/src/app/server_realtime_operations.rs`, `codex-rs/tui/src/app/server_control_operations.rs`, `codex-rs/tui/src/app/server_navigation_operations.rs`, `codex-rs/tui/src/app/server_navigation_cleanup.rs`, `codex-rs/tui/src/app/server_detach_operations.rs`, `codex-rs/tui/src/app/server_directory_navigation.rs`, `codex-rs/tui/src/app/server_fork_navigation.rs`, `codex-rs/tui/src/app/server_navigation_permissions.rs`.
- Tripwire: `pending_approval_survives_transcript_eviction` (`codex-rs/tui/src/app/tests/thread_delivery_tests.rs`); `stalled_submission_does_not_block_queued_edit_or_typing` (`codex-rs/tui/src/app/tests/queued_message_tests.rs`) composes response, control, draft, consent, configuration and canceled-acquisition custody; `changing_directory_preserves_project_trust_permissions_history_and_hooks` (`codex-rs/tui/src/app/tests/session_lifecycle_requests.rs`); `safety_retry_preserves_unconfirmed_permissions_on_its_first_turn` (`codex-rs/tui/src/app/tests/safety_buffering.rs`). Queued edits across a reconnect: none.
- Pressure: 0.162 #51415 edits `pending_interactive_replay.rs`; `reconnect.rs` takes 7 upstream commits (#49472, #49715, #49809, #49857, #50148, #50199, #51500).
- Evidence: b56191783c, 2926fc7aa0, 0aaf58f10c, c201932da2, 9a8bc53195, 8773e03b0d, 13c6bad38d, 6bc6d40a59, 64a630c9f0, cbed4dd438, 186954aa13.

## Resilience policy

### compaction-is-not-input

- Telos: compaction preserves enough handling evidence to continue without repeating already-handled or superseded work.
- Holds: a bounded factual record comes from the frozen compaction input, retaining recent requests, explicit goal changes, assistant reports and paired tool receipts. Acknowledgment is not completion and missing evidence proves nothing. Only host annotations authenticate a retained record; quoted user/tool wrappers do not. Replacement retains concurrent history, and malformed authenticated evidence returns an error without replacing history. Persisted record framing does not depend on reminder wording. Refreshed standing instructions remain separate from this evidence.
- Owners: `codex-rs/core/src/context/compaction_continuation.rs`, `codex-rs/core/src/compact.rs`, `codex-rs/core/src/compact_remote_v2.rs`, `codex-rs/core/src/compact_native.rs`.
- Tripwire: `native_checkpoint_swap_retains_concurrent_tail_and_rejects_stale_prefix_atomically` (`codex-rs/core/src/session/native_compaction_tests.rs`), `remote_compact_v2_retains_metadata_from_resumed_history` and the existing goal/guardian compaction interactions.
- Pressure: upstream summary-only replacement, retained-history filtering or provider-specific checkpoint installation must not turn compaction into new user intent or discard the host-authenticated continuation.
- Evidence: 2387ddf521, fabf777aac.

### backpressure-waits

- Telos: provider backpressure delays interactive work instead of failing it.
- Holds: `ServerOverloaded`, `RateLimitExceeded` and a 429 that is neither a usage limit nor exhausted quota make an interactive session wait as long as the user allows, without spending the retry budget; server advice sets the delay, otherwise it doubles from 5 s to 60 s; usage limits and exhausted quota end the turn; internal and Guardian sessions keep their failure path; Anthropic overload, rate-limit and API errors before any completed item stay retryable.
- Owners: `codex-rs/core/src/backpressure.rs`, `codex-rs/core/src/responses_retry.rs`, `codex-rs/codex-api/src/anthropic/decoder.rs`, `codex-rs/codex-api/src/anthropic/client.rs`.
- Tripwire: `responses_http_capacity_recovers_and_remains_interruptible`, `sse_rate_limit_without_retry_after_outlasts_stream_retries`, `sse_capacity_recovers` and `websocket_backpressure_recovers` (`codex-rs/core/tests/suite/retry_after.rs`), `errors_before_any_completed_item_stay_typed_for_retry` (`codex-rs/codex-api/src/anthropic/stream_tests.rs`).
- Pressure: 0.162 #49441 retries `ServerOverloaded` with server advice "within the configured retry budgets" in `responses_retry.rs`.
- Evidence: f36483414d, 076db2fb39.

### compaction-stops-on-uncertainty

- Telos: a compaction that may have succeeded, or failed for good, is not repeated on another model.
- Holds: `should_retry_with_current_model` is false for `Fatal`, `TurnAborted`, `Interrupted` and `SessionBudgetExceeded`; `compact_remote_v2` returns such an error, including one from the fallback attempt, instead of retrying with the current model.
- Owners: `codex-rs/core/src/compact_model_fallback.rs` (`should_retry_with_current_model`), `codex-rs/core/src/compact_remote_v2.rs`.
- Tripwire: none; its test left with `compact_remote_v2_native_tests.rs` in d68f5c1e02.
- Pressure: 0.162 #51117 edits `compact_model_fallback.rs` and `compact_remote_v2.rs`.
- Evidence: a8b367fbfe.

## Accounts

### pinned-accounts

- Telos: each conversation bills the account the human chose, and switching never breaks work in progress.
- Holds: every conversation and family is pinned to an account; a switch applies per session at the next safe request boundary without interrupting tools or inference; quota snapshots are shared through the server, coalesced, and honor per-account cooldowns; a fresh draft reads quota for the account it displays.
- Owners: `codex-rs/core/src/session/account.rs`, `codex-rs/app-server/src/request_processors/thread_accounts.rs`, `codex-rs/app-server/src/request_processors/account_processor/quota.rs`, `codex-rs/app-server/src/request_processors/account_processor/quota_cache.rs`, `codex-rs/app-server/src/request_processors/account_processor/account_scope.rs`, `codex-rs/tui/src/app_server_session/account.rs`.
- Tripwire: `conversation_accounts_survive_selection_login_and_fork` (`codex-rs/app-server/tests/suite/v2/thread_accounts.rs`), `cancelled_clients_cannot_erase_refreshes_or_failure_cooldowns` (`codex-rs/app-server/src/request_processors/account_processor/quota_cache_tests.rs`). Draft quota scope: none.
- Pressure: 0.162 #50199 (account email in `/status`) and #49715 (account security reminders) edit the TUI account paths in `app.rs`, `reconnect.rs` and `lib.rs`.
- Evidence: 8bc33e5acd, 0ec65fe8c4, df24f7a816, 9733575fc3.

## Repository policy

### policy-owned-worktrees

- Telos: checkout policy belongs to each repository, not to a competing Git worktree manager inside Codex. Agents use ordinary Git commands under the user's and repository's instructions.
- Holds: Codex neither allocates nor deletes managed Git worktrees, offers worktree commands or a browser, nor couples conversation creation or `/fork` to checkout creation. Conversation history forks, ordinary working-directory selection and execution of Git commands remain supported. Branch choice, detached state, checkout placement and retention are repository policy.
- Owners: `Feature::Worktrees` is a removed capability. No dedicated worktree crate, TUI lifecycle service, browser, checkout picker, CLI option or model tool remains. Unrelated Git inspection and generic attachment support remain.
- Tripwire: worktree commands and dependencies must not return. Conversation forks and ordinary cwd selection retain history, trust, permissions and background-terminal safeguards; thread pickers match the chosen cwd rather than expanding it to every linked checkout.
- Pressure: 0.162 #50148 adds model-facing managed-worktree tools hosted by the TUI; #50416 expands checkout choices for new and forked conversations. Neither should restore a product-owned worktree lifecycle.
- Evidence: maintainer decision on 2026-10-11 during 0.162.1 merge triage; CLI-wide locked checks and focused exact-cwd picker checks after removal.

## Build economy

### one-dependency-graph

- Telos: `cargo test` reuses the units `cargo build` compiled instead of compiling a second copy.
- Holds: no dev-dependency changes the features of a normal build unit; `codex-feature-floor` raises any feature a dev-dependency would otherwise add.
- Owners: `codex-rs/feature-floor/Cargo.toml`, `scripts/check_feature_parity.py`, `codex-rs/Cargo.toml`.
- Tripwire: `scripts/check_feature_parity.py`, run by hand; `just test` does not run it.
- Pressure: every upstream dependency change; 0.162 edits `codex-rs/Cargo.toml` 9 times, including #50113 (native gRPC client) and #50166 (`age` 0.12.1).
- Evidence: dec3c727fc.

### pipelined-crate-graph

- Telos: an edit recompiles as few crates as possible and they compile in parallel.
- Holds: `codex_cli` is a library and the `codex` binary a shim; the TUI and `codex-app-server-client` reach the app server only through `codex-app-server-embedding`, so no standalone `codex-tui` binary exists; core suites spawn `codex-exec`, not `codex`; the `local` profile builds workspace crates at O1 and dependencies at O2.
- Owners: `codex-rs/cli/src/main.rs`, `codex-rs/cli/src/bin/codex.rs`, `codex-rs/app-server-embedding/src/lib.rs`, `codex-rs/app-server/src/embedding.rs`, `codex-rs/app-server-client/Cargo.toml`, `scripts/run_tests.py`, `codex-rs/Cargo.toml` (`[profile.local]`).
- Tripwire: none.
- Pressure: 0.162 #50803, #50437 and #49939 edit `cli/src/main.rs`; any upstream dependency from the TUI or `codex-app-server-client` on `codex-app-server` restores the serial edge.
- Evidence: e092722ce7, 1064ccccc6, fad9f8bc86, 639630bf4e.

### removed-stacks

- Telos: capabilities this workstation never uses cost no compile time and no merge surface.
- Holds: the workspace and `codex-rs/Cargo.lock` contain none of: native Amazon Bedrock providers, credentials and onboarding; the OpenTelemetry SDK and exporters (`opentelemetry` remains with `trace` only); the managed network-proxy transport and Rama; AWS SigV4 and `codex-aws-auth`; the voice host and its WebRTC and ALSA graph; `tcp-tunnel`; `responses-api-proxy`; `app-server-test-client`; arboard's Wayland backend; `thread-manager-sample` and `core-api`; Windows sandbox dependencies outside `cfg(windows)` targets. `sdk/typescript`, its npm dependency graph, build/test integration and publication artifact are removed. Shared TypeScript protocol generators, the Python SDK, Rust APIs, headless execution and CLI packaging remain. Shared MCP conformance fixtures remain in Python CI; the SDK-hosted official and production reviewer CI gates are retired. Manual conformance requires an externally provisioned runner and never bootstraps registry dependencies.
- Owners: none; the deletions leave seams in `codex-rs/Cargo.toml`, `codex-rs/cli/Cargo.toml`, `codex-rs/cli/src/main.rs`, `codex-rs/windows-sandbox-rs/Cargo.toml` and `codex-rs/mxc-sandbox/Cargo.toml`.
- Tripwire: none; needs a denylist check of the deleted directories, of `Cargo.lock` packages (`opentelemetry_sdk`, `opentelemetry-otlp`, `rama*`, `aws-sigv4`, `webrtc*`, `alsa`, `wayland*`) and of arboard features.
- Pressure: 42 files deleted on main are modified upstream between `rust-v0.160.0` and `rust-v0.162.0`, chiefly under `model-provider/src/amazon_bedrock`, `voice-host`, `tcp-tunnel` and `otel/tests`; each is a modify/delete conflict whose resolution is `git rm`. 0.162 #49836 adds voice microphone selection to `core/src/config/mod.rs`.
- Evidence: 94d7235008, 3f4d858769, 5f45f5bbc5, 0700355c3e, 7f2715c164, 60f6ba0573, e176b09c07, 6d7c491d67, bbf73666b7, 177ca0e1e2, d206c23b79, 262a393d8a.

## TUI doctrine

### client-only-shell

- Telos: a deliberate local terminal command need not enter the agent's conversation or wait for its server.
- Holds: only visible composer `!!` intent admits host-local execution; hidden pasted prefixes and attachments cannot acquire it. One admission lease owns the command before the draft is cleared, then transfers to the process job or retained rejection. Host cwd, local process-group custody, bounded output and cancellation stay separate from agent shell policy and conversation cwd. Startup recovery never turns local intent into agent input; newer drafts are not overwritten by failure recovery.
- Owners: `codex-rs/tui/src/local_shell.rs`, `codex-rs/tui/src/app/local_shell.rs`, `codex-rs/tui/src/bottom_pane/chat_composer/local_shell.rs`, `codex-rs/tui/src/chatwidget/local_shell.rs`.
- Tripwire: `pasted_input_preserves_app_boundary_and_local_shell_authority` (`codex-rs/tui/src/app/tests.rs`).
- Pressure: composer paste handling, submission/reconnect recovery and shell routing must preserve the explicit host-local authority boundary and single draft lease.
- Evidence: 865ddc1d21, 1103c818cc, 4995ef537e.

### explicit-command-verbs

- Telos: a mistyped command never becomes an instruction to execute or a value to store.
- Holds: a command with no subcommands may accept bare prose; if subcommands exist, all are explicit. Enumerated verbs reject unknowns before executing, queuing work, or changing goal, conversation, or draft state. Prose at a verb-bearing boundary requires its explicit verb. Bare menu/status affordances remain valid. `/goal set <objective>` alone enters the objective path; `/goal` opens the existing menu, and replacing an unfinished goal still requires confirmation. Use the existing typed enum/parsing machinery, not a second generic command framework.
- Owners: `codex-rs/tui/src/chatwidget/goal_command.rs` (`GoalCommand`, `ChatWidget::dispatch_goal_command`), `codex-rs/tui/src/chatwidget/context_role.rs` (`ContextRoleCommand`), `codex-rs/tui/src/chatwidget/pets_command.rs` (`PetCommand`), `codex-rs/tui/src/chatwidget/slash_dispatch.rs`, `codex-rs/tui/src/goal_display.rs`, `codex-rs/tui/src/slash_command.rs`, `codex-rs/exec/src/command_grammar.rs` (`Command`), `codex-rs/cli/src/sandbox_command.rs` (`SandboxSubcommand`); top-level CLI prose is already explicit through `codex --prompt` in `codex-rs/tui/src/cli.rs`.
- Tripwire: `goal_control_slash_commands_emit_goal_events`, `goal_slash_command_emits_attached_images`, `queued_goal_slash_command_emits_set_goal_event_after_thread_starts`, `restored_queued_goal_slash_command_emits_set_goal_event`, `slash_pets_with_arg_selects_named_pet` (`codex-rs/tui/src/chatwidget/tests/slash_commands.rs`), `context_role_command_output` (`codex-rs/tui/src/chatwidget/tests/context_role_tests.rs`), `goal_slash_command_emits_only_inserted_paste_text_element` (`codex-rs/tui/src/chatwidget/tests/goal_validation.rs`), `set_thread_goal_draft_materializes_long_objective_and_confirms_before_paste` (`codex-rs/tui/src/app/tests.rs`), `parses_config_isolation_flags` (`codex-rs/exec/src/cli_tests.rs`), stdin interactions (`codex-rs/exec/tests/suite/prompt_stdin.rs`), `unrecognized_words_are_rejected_rather_than_seeded_as_prompts` and `sandbox_uninstall_respects_command_separator` (`codex-rs/cli/src/main.rs`).
- Pressure: upstream `/goal` treats every unrecognized control word as a new objective; upstream's CLI also allows bare positional prompts. Preserve explicit verbs and the confirmation path when merging dispatch, help, or completion changes.
- Evidence: command-grammar owner decision, 2026-10-12.

### fullscreen-only

- Telos: the fork maintains one frontend, the owned screen.
- Holds: no inline-renderer parity; the transcript flows from events through an owned reducer into immutable cells, and replacements go only through `App::commit_transcript_splice`; each tool history header is one compact row with source-safe disclosure; clocks and token bills sit in a fixed leading gutter; Code Mode blocks render their owned children. Human-only live work belongs below the transcript rule: current-agent operations and alarms, plus owner-labelled managed processes from its descendant tree. Disclosures determine height without an internal scrollbar or arbitrary row cap. `/ps` opens this inventory rather than appending history. Liveness comes from runtime snapshots and events, not rendered transcript text; completed work leaves the live view without erasing its durable outcome.
- Owners: `AGENTS.md`, `codex-rs/tui/src/app/transcript_splice.rs`, `codex-rs/tui/src/exec_cell/compact.rs`, `codex-rs/tui/src/transcript_view/text_activity.rs`, `codex-rs/tui/src/transcript_view/text_gutter.rs`, `codex-rs/tui/src/history_cell/code_mode.rs`, `codex-rs/tui/src/history_cell/code_mode_projection.rs`.
- Tripwire: gutter: `token_gutter_rounding_color_and_reflow_preserve_source` (`codex-rs/tui/src/transcript_view/text_tests.rs`), `tool_output_disclosure_shows_fixed_token_column` (`codex-rs/tui/src/transcript_view/layout_tests.rs`). Headers and Code Mode blocks: noisy (`codex-rs/tui/src/**/snapshots/*.snap`). Splice-only replacement: none.
- Pressure: 0.162 #50434 adds keyboard copy selection to the owned transcript; #50808 prunes TUI snapshots; #50109 bounds fullscreen prompts.
- Evidence: 1cf770bb6b, b78ff3aff3, eaefb734ab, ae20fc5bef, cdc2ec4845.

### fast-ui-thread

- Telos: typing, navigation and cancellation remain responsive while agent work, history updates and rendering preparation progress.
- Holds: required invariant: the UI dispatcher performs bounded state transitions and cached presentation; it never awaits server responses or performs unbounded I/O or source preparation. Server, configuration and filesystem work runs as typed continuations: the dispatcher admits the request, and its completion returns the outcome to the UI task. Ordering, input ownership and uncertain outcomes are preserved, unrelated editing is never held, and a request that snapshots settings takes its snapshot behind the settings barrier. Each loop pass drains a finite input batch before frame work and polls terminal input with the UI task's own waker. An output writer thread owns the terminal: frames are latest-wins, controls and barriers keep their order, the next frame paints only after the previous one completes, and a geometry change or clear invalidates the writer's previous frame. Cell layout and streaming Markdown are prepared on background workers and published as immutable revisions; sequential parsing, semantic formatting, highlighting and token counting finish before publication. The screen holds the presented revision until its successor is ready and never re-renders the same content in place; a width change blanks the transcript region rather than showing clipped rows, and the composer always renders. Unchanged source preparation is reused across headers, bodies and cosmetic ticks; animation repaints without rebuilding content. Terminals that confirm bracketed paste skip the paste-burst hold.
- Owners: `codex-rs/tui/src/app/startup.rs`, `codex-rs/tui/src/app/ui_schedule.rs`, `codex-rs/tui/src/app/server_ui_requests.rs`, `codex-rs/tui/src/app/local_ui_requests.rs`, `codex-rs/tui/src/app/navigation_config.rs`, `codex-rs/tui/src/custom_terminal/output.rs`, `codex-rs/tui/src/transcript_view/presented.rs`, `codex-rs/tui/src/streaming/background.rs`, `codex-rs/tui/src/app/owned_transcript.rs`, `codex-rs/tui/src/transcript_view/invocation.rs`, `codex-rs/tui/src/history_cell/invocation.rs`, `codex-rs/tui/src/history_cell/activity_lifecycle.rs`, `codex-rs/tui/src/transcript_view/layout.rs`, `codex-rs/tui/src/ui_latency.rs` and its endpoint, `codex-rs/cli/src/ui_latency.rs`.
- Tripwire: `blocked_writer_keeps_ui_live_and_preserves_latest_frame_and_barrier_order` stalls the writer, then checks live input, latest-frame delivery, barrier order, and repaint after width changes and clears. `controller_live_view_matches_render_during_interleaved_table_streaming` checks worker-published stream rows against canonical rendering; a setext underline or a late table pipe restyles rows already shown, through a keyed splice. `invocation_ports_preserve_sources_across_completion_and_reflow` checks preparation reuse across cosmetic ticks and unrelated call arrivals. `script_panes_settle_off_dispatcher_and_reject_stale_completions` holds the preparation worker while exercising pane controls, then checks stale-width rejection and settled live reuse. Script panes publish highlighted, wrapped copy geometry together, never a half-prepared source. `mcp_file_receipts_share_native_hunks_and_independent_disclosures` checks native projections. No in-repo latency bound: only an external pty harness checks real terminal input wake-ups and paint.
- Diagnostics: UI-task-local, fixed-stage wall-time recording publishes current work before execution. Its dedicated worker-local socket thread never depends on dispatcher/server/runtime progress; snapshots are bounded and reset is a lazy reporting epoch, not cancellation. No content, hot-path allocation, logging or disk export. Ordinary idle select is distinct from handler occupancy; timing remains inclusive, not CPU attribution. `ReactionLatency` spans input consumption to the flushed frame. Operator contract: [UI latency recorder](fork.md#ui-latency-recorder). `independent_dump_survives_blocked_and_suspended_dispatcher` and `publication_reset_race_is_bounded_and_never_accepts_torn_records` witness isolation and bounded publication/reset; they do not establish an end-to-end latency bound.
- Pressure: upstream handlers that await the server, and rendering changes that prepare on the dispatcher, reintroduce stalls; convert them to continuations or worker preparation. Crossterm's event stream latches the waker of the poll that armed its reader, so a noop-waker poll strands the UI task. A synchronous terminal query (cursor position, colors) stalls the UI until the terminal answers or the probe times out, so after startup an owned screen issues none. Only the UI task publishes current recorder work; writer-thread phases enter histograms through drained completions.
- Evidence: owner decision, 2026-10-15; responsive-ui acceptance, 2026-10-17.

### attention-gated-painting

- Telos: an unwatched TUI spends no CPU on painting.
- Holds: `PresentationMode` (`Normal`, `Reduced`, `Frozen`) follows X11 idle time and i3 visibility and focus (`DesktopAttention`); unknown visibility or focus never counts as hidden; `Reduced` and `Frozen` stop animation and title refresh; attention never suspends agent or server work; observer failure restores a usable UI.
- Owners: `codex-rs/tui/src/tui/presentation.rs` (`DesktopAttention`), `codex-rs/tui/src/tui/desktop_attention.rs`, `codex-rs/tui/src/tui/frame_requester.rs` (`PresentationMode`), `codex-rs/tui/src/app/presentation.rs`.
- Tripwire: `observer_freeze_retires_presentation_clocks_without_suspending_input` (`codex-rs/tui/src/app/tests.rs`), `reduced_presentation_keeps_status_readable_without_activity_animation` (`codex-rs/tui/src/status_indicator_widget/effects_tests.rs`).
- Pressure: none in 0.162 on the owners.
- Evidence: 5247a0733e.

### key-doctrine

- Telos: keys mean one thing each, and none destroys work by accident.
- Holds: the default keymap binds `chat.interrupt_turn` to Ctrl+C and `chat.send_pending_steer` to Alt+I; Esc navigates and by default interrupts nothing; Ctrl+C keeps the draft; Up/Down recall only the current conversation's prompts; a mouse selection is published to PRIMARY and reaches CLIPBOARD only under copy-on-select.
- Owners: `codex-rs/tui/src/keymap.rs`, `codex-rs/tui/src/keymap/bindings.rs`, `codex-rs/config/src/tui_keymap.rs`, `codex-rs/tui/src/chatwidget/interaction.rs`, `codex-rs/tui/src/bottom_pane/chat_composer_history.rs`, `codex-rs/tui/src/transcript_view/selection.rs`, `codex-rs/tui/src/clipboard_copy.rs`.
- Tripwire: `owned_details_ctrl_c_interrupts_work_without_starting_backtrack` (`codex-rs/tui/src/app/owned_transcript_tests.rs`), `alt_i_sends_pending_steer_immediately_and_keeps_existing_draft` (`codex-rs/tui/src/chatwidget/tests/review_mode.rs`), `copy_on_select_preserves_link_activation_and_ignores_empty_drags` (`codex-rs/tui/src/transcript_view/copy_on_select_tests.rs`). Recall boundary: noisy (`conversation_prompt_recall_boundary` snapshot in `codex-rs/tui/src/chatwidget/tests/history_replay.rs`).
- Pressure: 0.162 #50389 (configured keybindings before transcript navigation), #49517 (fork shortcut) and #51500 (task pinning) edit `keymap.rs` and `tui_keymap.rs`; #50467 edits `clipboard_copy.rs`.
- Evidence: 86600d9708, d174f70315, 39fba99ad1, f36947b64a.

### async-questions-never-auto-answered

- Telos: an agent's question is answered only by the human.
- Holds: the TUI delivers an answer only as text the user wrote, through ordinary user-turn delivery; opening, leaving, skipping or dismissing the question picker neither interrupts work nor synthesizes an answer.
- Owners: `codex-rs/tui/src/chatwidget/questions.rs`, `codex-rs/tui/src/async_question_reply.rs`, `codex-rs/tui/src/bottom_pane/request_user_input/mod.rs`.
- Tripwire: partial: `codex-rs/tui/src/chatwidget/tests/questions_tests.rs` asserts the ops that answers and follow-ups send; no test asserts that leaving or dismissing sends nothing.
- Pressure: upstream's async-question integration replaced the fork's implementation at the 0.154.0 merge (06ae488041), so every owner is upstream code; 0.162 #51449 and #49804 edit `request_user_input/mod.rs`.
- Evidence: 193bbc9b49.

## Merge hygiene

### minimal-upstream-delta

- Telos: each upstream merge conflicts only where the fork has a reason to differ.
- Holds: an upstream file differs from upstream only by seams (module declaration, call, match arm, field holding fork state); `.github/workflows/rust-ci-full-nextest-platform.yml` and `.github/workflows/rust-release.yml` equal their bytes at the merged release tag; fork instructions live in fork-owned `AGENTS.md`; a stub keeps upstream call sites unchanged.
- Owners: `AGENTS.md`.
- Tripwire: none; `git diff --quiet <release tag> HEAD -- .github/workflows/rust-ci-full-nextest-platform.yml .github/workflows/rust-release.yml` would be one.
- Pressure: every upstream merge.
- Evidence: 311bcd0f11, 69467ed11f, e4b9e0a34f, 6aa3fee801.

## Dev hygiene

### hermetic-test-runs

- Telos: a test run cannot read or disturb live Codex state.
- Holds: `just test` runs nextest through `scripts/run_tests.py`, which gives each run an empty `CODEX_HOME`, private directories bound with `bwrap` over `/run/user/<uid>/codex-1337` and `/tmp/codex-daemon-<uid>`, and disk-backed scratch that only that run deletes, and builds the binaries its selected tests spawn before running them.
- Owners: `scripts/run_tests.py`, `justfile`.
- Tripwire: scratch ownership: `test_runs_clean_only_their_own_scratch_after_exit_or_cancel` (`scripts/test_run_tests.py`, run by hand). Empty home, private runtime directories and prebuilt binaries: none.
- Pressure: none in 0.162 on the owners.
- Evidence: 1112d83c56, a365185343, dbe071b9e1, a53d7705fe.

## Index

Upstream files, where merge conflicts arise:

- `codex-rs/features/src/lib.rs`: append-only-tool-declarations
- `codex-rs/core/src/session/step_context.rs`: append-only-tool-declarations
- `codex-rs/core/src/session/world_state.rs`: append-only-tool-declarations, standing-context-trims
- `codex-rs/Cargo.toml`: one-dependency-graph, pipelined-crate-graph, removed-stacks
- `codex-rs/app-server-client/Cargo.toml`: pipelined-crate-graph
- `codex-rs/app-server/src/request_processors/thread_queue_processor.rs`: server-hosted-agents
- `codex-rs/app-server/src/turn_admission.rs`: shared-daemon
- `codex-rs/cli/Cargo.toml`: removed-stacks
- `codex-rs/cli/src/main.rs`: pipelined-crate-graph, removed-stacks, explicit-command-verbs, managed-process-visibility
- `codex-rs/exec/src/cli.rs`: explicit-command-verbs
- `codex-rs/exec/src/lib.rs`: explicit-command-verbs
- `codex-rs/tui/src/cli.rs`: explicit-command-verbs
- `codex-rs/code-mode-protocol/src/description.rs`: mcp-roster, standing-context-trims, local-coprocessor, response-order-fences
- `codex-rs/codex-mcp/src/binding.rs`: standing-context-trims
- `codex-rs/codex-mcp/src/client_tool_catalog.rs`: catalog-at-model-boundaries
- `codex-rs/codex-mcp/src/connection_manager.rs`: standing-context-trims
- `codex-rs/codex-mcp/src/connection_manager/tool_catalog.rs`: catalog-at-model-boundaries
- `codex-rs/codex-mcp/src/runtime.rs`: catalog-at-model-boundaries
- `codex-rs/codex-mcp/src/tool_catalog_cache.rs`: catalog-at-model-boundaries
- `codex-rs/codex-mcp/src/tools.rs`: catalog-at-model-boundaries
- `codex-rs/collaboration-mode-templates/templates/default.md`: standing-context-trims
- `codex-rs/config/src/tui_keymap.rs`: key-doctrine
- `codex-rs/core/src/agent/control.rs`: server-hosted-agents
- `codex-rs/core/src/compact_model_fallback.rs`: compaction-stops-on-uncertainty
- `codex-rs/core/src/compact_remote_v2.rs`: compaction-stops-on-uncertainty, compaction-is-not-input
- `codex-rs/core/src/config/mod.rs`: adapter-not-transport
- `codex-rs/core/src/context/world_state/mod.rs`: retained-goal-rules
- `codex-rs/core/src/mcp_tool_call.rs`: human-only-facts, catalog-at-model-boundaries
- `codex-rs/core/src/responses_retry.rs`: backpressure-waits
- `codex-rs/core/src/session/context_window.rs`: adapter-not-transport
- `codex-rs/core/src/session/turn.rs`: native-robustness, response-order-fences
- `codex-rs/core/src/stream_events_utils.rs`: native-robustness, response-order-fences
- `codex-rs/core/src/tools/code_mode/delegate.rs`: response-order-fences
- `codex-rs/core/src/tools/code_mode/execute_handler.rs`: compact-contracts, turn-economy
- `codex-rs/core/src/tools/code_mode/execute_spec.rs`: mcp-roster
- `codex-rs/core/src/tools/code_mode/output.rs`: terse-exec-envelope
- `codex-rs/core/src/tools/code_mode/wait_handler.rs`: turn-economy
- `codex-rs/core/src/tools/context.rs`: terse-exec-envelope
- `codex-rs/core/src/tools/handlers/mcp.rs`: compact-contracts
- `codex-rs/core/src/tools/handlers/shell_spec.rs`: standing-context-trims
- `codex-rs/core/src/tools/handlers/unified_exec/exec_command.rs`: standing-context-trims, local-coprocessor
- `codex-rs/core/src/tools/parallel.rs`: response-order-fences
- `codex-rs/core/src/tools/runtimes/unified_exec.rs`: local-coprocessor
- `codex-rs/core/src/tools/spec_plan.rs`: mcp-roster, standing-context-trims, turn-economy, local-coprocessor, response-order-fences
- `codex-rs/core/src/unified_exec/process_manager.rs`: local-coprocessor
- `codex-rs/ext/goal/src/extension.rs`: retained-goal-rules
- `codex-rs/ext/goal/src/runtime.rs`: retained-goal-rules
- `codex-rs/ext/goal/src/spec.rs`: standing-context-trims
- `codex-rs/ext/queue/src/service.rs`: server-hosted-agents
- `codex-rs/model-provider-info/src/lib.rs`: subscription-boundary, adapter-not-transport
- `codex-rs/mxc-sandbox/Cargo.toml`: removed-stacks
- `codex-rs/prompts/src/permissions_instructions.rs`: standing-context-trims
- `codex-rs/prompts/templates/permissions/approval_policy/on_request.md`: standing-context-trims
- `codex-rs/protocol/src/items.rs`: human-only-facts
- `codex-rs/protocol/src/models.rs`: native-robustness
- `codex-rs/rollout/src/policy.rs`: human-only-facts
- `codex-rs/tui/src/app/startup.rs`: fast-ui-thread
- `codex-rs/tui/src/app/owned_transcript.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/layout.rs`: fast-ui-thread
- `codex-rs/tui/src/custom_terminal.rs`: fast-ui-thread
- `codex-rs/tui/src/streaming/controller.rs`: fast-ui-thread
- `codex-rs/tui/src/app/event_dispatch.rs`: fast-ui-thread
- `codex-rs/tui/src/app_server_session.rs`: fast-ui-thread
- `codex-rs/tui/src/app/config_persistence.rs`: fast-ui-thread
- `codex-rs/tui/src/app/thread_goal_actions.rs`: fast-ui-thread
- `codex-rs/tui/src/goal_files.rs`: fast-ui-thread
- `codex-rs/tui/src/app/pending_interactive_replay.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/reconnect.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/thread_event_buffer.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/thread_routing.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/async_question_reply.rs`: async-questions-never-auto-answered
- `codex-rs/tui/src/bottom_pane/chat_composer_history.rs`: key-doctrine
- `codex-rs/tui/src/bottom_pane/request_user_input/mod.rs`: async-questions-never-auto-answered
- `codex-rs/tui/src/chatwidget/interaction.rs`: key-doctrine
- `codex-rs/tui/src/chatwidget.rs`: explicit-command-verbs
- `codex-rs/tui/src/chatwidget/slash_dispatch.rs`: explicit-command-verbs
- `codex-rs/tui/src/goal_display.rs`: explicit-command-verbs
- `codex-rs/tui/src/slash_command.rs`: explicit-command-verbs
- `codex-rs/tui/src/chatwidget/questions.rs`: async-questions-never-auto-answered
- `codex-rs/tui/src/clipboard_copy.rs`: key-doctrine
- `codex-rs/tui/src/exec_cell/compact.rs`: fullscreen-only
- `codex-rs/tui/src/keymap.rs`: key-doctrine
- `codex-rs/tui/src/keymap/bindings.rs`: key-doctrine
- `codex-rs/tui/src/lib.rs`: shared-daemon
- `codex-rs/tui/src/transcript_view/selection.rs`: key-doctrine
- `codex-rs/tui/src/tui/frame_requester.rs`: attention-gated-painting, fast-ui-thread
- `codex-rs/windows-sandbox-rs/Cargo.toml`: removed-stacks
- `justfile`: hermetic-test-runs
- `codex-rs/protocol/src/tool_name.rs`: canonical-tool-identity
- `codex-rs/core/src/tools/registry.rs`: startup-tool-ceiling
- `codex-rs/tui/src/temporary_structured_request.rs`: startup-tool-ceiling
- `codex-rs/sandboxing/src/linux_pid_namespace.rs`: managed-process-visibility
- `codex-rs/core/src/compact.rs`: compaction-is-not-input
- `codex-rs/thread-store/src/local/revert_thread.rs`: accepted-agent-mail
- `codex-rs/core/src/session/handlers.rs`: accepted-agent-mail
- `codex-rs/tui/src/bottom_pane/chat_composer.rs`: client-only-shell
- `codex-rs/app-server/src/lib.rs`: managed-process-visibility, startup-tool-ceiling

- `codex-rs/tui/src/chatwidget/streaming.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view.rs`: fast-ui-thread
- `codex-rs/tui/src/terminal_probe.rs`: fast-ui-thread
- `codex-rs/tui/src/tui/event_stream.rs`: fast-ui-thread
- `codex-rs/tui/src/tui.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/layout_tests.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/input.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/disclosure.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/snapshot.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/search.rs`: fast-ui-thread
- `codex-rs/tui/src/startup_draft.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/base.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/messages.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/session.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/mcp.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/plans.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/notices.rs`: fast-ui-thread

Fork-owned files, absent upstream:
- `codex-rs/tui/src/tui/frame_requester/paint_schedule.rs`: fast-ui-thread
- `codex-rs/tui/src/tui/frame_rate_limiter/completion.rs`: fast-ui-thread
- `codex-rs/tui/src/tui/event_stream/preparation.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/text/preparation.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/layout_preparation_tests.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/layout_preparation.rs`: fast-ui-thread
- `codex-rs/tui/src/terminal_probe/imp/tests/bracketed_paste_tests.rs`: fast-ui-thread
- `codex-rs/tui/src/streaming/controller/tests/background_tests.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/session/snapshot.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/messages/snapshot.rs`: fast-ui-thread
- `codex-rs/tui/src/chatwidget/activity_presentation/live_snapshot.rs`: fast-ui-thread
- `codex-rs/tui/src/app/owned_transcript_preparation_tests.rs`: fast-ui-thread
- `codex-rs/tui/src/app/owned_transcript_input_preparation.rs`: fast-ui-thread

- `codex-rs/tui/src/transcript_view/invocation.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/invocation.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/activity_lifecycle.rs`: fast-ui-thread
- `codex-rs/tui/src/app/ui_schedule.rs`: fast-ui-thread
- `codex-rs/tui/src/app/server_ui_requests.rs`: fast-ui-thread
- `codex-rs/tui/src/app/local_ui_requests.rs`: fast-ui-thread
- `codex-rs/tui/src/app/navigation_config.rs`: fast-ui-thread
- `codex-rs/tui/src/app/prompt_revert.rs`: fast-ui-thread
- `codex-rs/tui/src/custom_terminal/output.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/presented.rs`: fast-ui-thread
- `codex-rs/tui/src/live_work/presented.rs`: fast-ui-thread
- `codex-rs/tui/src/streaming/background.rs`: fast-ui-thread
- `AGENTS.md`: fullscreen-only, minimal-upstream-delta
- `codex-rs/app-server-embedding/src/lib.rs`: pipelined-crate-graph
- `codex-rs/app-server-protocol/src/delegation.rs`: server-hosted-agents
- `codex-rs/app-server-protocol/src/protocol/v2/prompt_cache.rs`: cache-ttl-and-warming
- `codex-rs/app-server-protocol/src/protocol/v2/thought.rs`: token-bills
- `codex-rs/app-server/src/embedding.rs`: pipelined-crate-graph
- `codex-rs/app-server/src/prompt_cache.rs`: cache-ttl-and-warming
- `codex-rs/app-server/src/request_processors/account_processor/account_scope.rs`: pinned-accounts
- `codex-rs/app-server/src/request_processors/account_processor/quota.rs`: pinned-accounts
- `codex-rs/app-server/src/request_processors/account_processor/quota_cache.rs`: pinned-accounts
- `codex-rs/app-server/src/request_processors/comms_gate.rs`: server-hosted-agents
- `codex-rs/app-server/src/request_processors/context_role.rs`: server-hosted-agents
- `codex-rs/app-server/src/request_processors/thread_accounts.rs`: pinned-accounts
- `codex-rs/app-server/src/request_processors/thread_warming.rs`: cache-ttl-and-warming
- `codex-rs/app-server/src/shutdown.rs`: shared-daemon
- `codex-rs/app-server/src/thread_tools/mod.rs`: server-hosted-agents
- `codex-rs/cli/src/bin/codex.rs`: pipelined-crate-graph
- `codex-rs/cli/src/reload_probe.rs`: reload-host
- `codex-rs/cli/src/sandbox_command.rs`: explicit-command-verbs
- `codex-rs/exec/src/command_grammar.rs`: explicit-command-verbs
- `codex-rs/code-mode-protocol/src/contract.rs`: compact-contracts
- `codex-rs/code-mode-protocol/src/presentation.rs`: compact-contracts, mcp-roster
- `codex-rs/code-mode-protocol/src/discovery.rs`: compact-contracts
- `codex-rs/code-mode-protocol/src/roster.rs`: mcp-roster
- `codex-rs/code-mode-runtime/src/runtime/discovery.rs`: compact-contracts
- `codex-rs/code-mode-runtime/src/runtime/filesystem.rs`: local-coprocessor
- `codex-rs/code-mode-runtime/src/runtime/native_result.rs`: terse-exec-envelope
- `codex-rs/codex-api/src/anthropic/cache.rs`: cache-ttl-and-warming
- `codex-rs/codex-api/src/anthropic/client.rs`: backpressure-waits
- `codex-rs/codex-api/src/anthropic/decoder.rs`: native-robustness, backpressure-waits
- `codex-rs/codex-api/src/anthropic/history.rs`: reminders-not-system, stable-native-identities, messages-from-canonical-history
- `codex-rs/codex-api/src/anthropic/limits.rs`: native-robustness
- `codex-rs/codex-api/src/anthropic/request.rs`: claude-native-voice, subscription-boundary
- `codex-rs/codex-api/src/anthropic/salvage.rs`: messages-from-canonical-history
- `codex-rs/codex-api/src/anthropic/tool_alias.rs`: stable-native-identities
- `codex-rs/codex-api/src/anthropic/warm.rs`: cache-ttl-and-warming
- `codex-rs/config/src/anthropic_prompt_cache.rs`: cache-ttl-and-warming
- `codex-rs/core/src/agent/control/retirement.rs`: server-hosted-agents
- `codex-rs/core/src/agent/model_catalog.rs`: bare-model-slugs
- `codex-rs/core/src/backpressure.rs`: backpressure-waits
- `codex-rs/core/src/client_anthropic.rs`: subscription-boundary
- `codex-rs/core/src/client_anthropic_tools.rs`: claude-native-voice, stable-native-identities
- `codex-rs/core/src/compact_native.rs`: messages-from-canonical-history, compaction-is-not-input
- `codex-rs/core/src/compact_native_request.rs`: messages-from-canonical-history, native-robustness
- `codex-rs/core/src/config/models_cache.rs`: adapter-not-transport
- `codex-rs/core/src/context/environment_context/workspace_root_folding.rs`: standing-context-trims
- `codex-rs/core/src/context/base_instructions.rs`: instruction-amendments
- `codex-rs/core/src/context/world_state/instruction_diff.rs`: instruction-amendments
- `codex-rs/core/src/context/world_state/instruction_projection.rs`: instruction-amendments
- `codex-rs/core/src/context_manager/instruction_projection.rs`: instruction-amendments
- `codex-rs/core/src/exec_policy/script_exec.rs`: local-coprocessor
- `codex-rs/core/src/guardian/reviewer_route.rs`: provider-parity
- `codex-rs/core/src/session/account.rs`: pinned-accounts
- `codex-rs/core/src/session/input_queue/interjection.rs`: turn-economy
- `codex-rs/core/src/session/response_thought.rs`: human-only-facts, token-bills
- `codex-rs/core/src/session/preamble.rs`: token-bills
- `codex-rs/core/src/session/context_inventory.rs`: token-bills
- `codex-rs/core/src/session/tool_output_tokens.rs`: token-bills
- `codex-rs/core/src/tools/fence.rs`: response-order-fences
- `codex-rs/core/src/tools/handlers/code_mode_fs.rs`: local-coprocessor
- `codex-rs/core/src/tools/handlers/multi_agents_v2/retire_agent.rs`: server-hosted-agents
- `codex-rs/core/src/session/input_queue/mailbox_batch.rs`: turn-economy
- `codex-rs/core/src/tools/handlers/script_exec/`: local-coprocessor
- `codex-rs/core/src/tools/handlers/unified_exec/exec_command/cell.rs`: turn-economy
- `codex-rs/core/src/tools/handlers/unified_exec/write_stdin/cell.rs`: turn-economy
- `codex-rs/core/src/tools/handlers/work_status.rs`: human-only-facts
- `codex-rs/core/src/tools/script_exec.rs`: local-coprocessor
- `codex-rs/core/src/unified_exec/initial_wait.rs`: turn-economy
- `codex-rs/ext/goal/src/world_state.rs`: retained-goal-rules
- `codex-rs/feature-floor/Cargo.toml`: one-dependency-graph
- `codex-rs/history/src/tool_failure.rs`: stable-native-identities
- `codex-rs/model-provider/src/claude_catalog.rs`: claude-native-voice
- `codex-rs/protocol/src/code_mode.rs`: human-only-facts
- `codex-rs/protocol/src/comms_gate.rs`: server-hosted-agents
- `codex-rs/protocol/src/context_role.rs`: server-hosted-agents
- `codex-rs/protocol/src/mcp_file_changes.rs`: human-only-facts
- `codex-rs/protocol/src/native_execution_result.rs`: terse-exec-envelope
- `codex-rs/protocol/src/preamble.rs`: human-only-facts, token-bills
- `codex-rs/state/src/runtime/farm_requests.rs`: server-hosted-agents
- `codex-rs/state/src/runtime/thread_names.rs`: server-hosted-agents
- `codex-rs/tui/src/app/idle_restart.rs`: reload-host
- `codex-rs/tui/src/app/presentation.rs`: attention-gated-painting
- `codex-rs/tui/src/app/queued_messages.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/transcript_splice.rs`: fullscreen-only
- `codex-rs/tui/src/app/ui_reload.rs`: reload-host
- `codex-rs/tui/src/app_server_session/account.rs`: pinned-accounts
- `codex-rs/tui/src/chatwidget/goal_command.rs`: explicit-command-verbs
- `codex-rs/tui/src/chatwidget/context_role.rs`: explicit-command-verbs
- `codex-rs/tui/src/chatwidget/pets_command.rs`: explicit-command-verbs
- `codex-rs/tui/src/history_cell/code_mode.rs`: fullscreen-only
- `codex-rs/tui/src/history_cell/code_mode_projection.rs`: fullscreen-only
- `codex-rs/tui/src/history_cell/preamble.rs`: token-bills
- `codex-rs/tui/src/reload/admission.rs`: reload-host
- `codex-rs/tui/src/reload/deadline.rs`: reload-host
- `codex-rs/tui/src/reload/host.rs`: reload-host
- `codex-rs/tui/src/reload/mod.rs`: reload-host
- `codex-rs/tui/src/reload/retained.rs`: reload-host
- `codex-rs/tui/src/transcript_view/text_activity.rs`: fullscreen-only
- `codex-rs/tui/src/transcript_view/text_gutter.rs`: token-bills, fullscreen-only
- `codex-rs/tui/src/tui/desktop_attention.rs`: attention-gated-painting
- `codex-rs/tui/src/tui/presentation.rs`: attention-gated-painting
- `codex-rs/utils/output-truncation/src/token_count.rs`: token-bills
- `docs/fork.md`: provider-parity
- `scripts/check_feature_parity.py`: one-dependency-graph
- `scripts/run_tests.py`: pipelined-crate-graph, hermetic-test-runs
- `scripts/systemd/codex-app-server-refresh.path`: shared-daemon
- `scripts/systemd/codex-app-server-refresh.service`: shared-daemon
- `scripts/systemd/codex-app-server.service`: shared-daemon, managed-process-visibility


- `codex-rs/protocol/src/hook_selector.rs`: canonical-tool-identity
- `codex-rs/hooks/src/engine/selection.rs`: canonical-tool-identity
- `codex-rs/core/src/tool_policy.rs`: canonical-tool-identity, startup-tool-ceiling
- `codex-rs/protocol/src/tool_policy.rs`: startup-tool-ceiling



- `codex-rs/app-server/src/local_exec.rs`: managed-process-visibility


- `scripts/systemd/codex-app-server-staging.service`: managed-process-visibility
- `codex-rs/core/src/agent/control/accepted_mail.rs`: accepted-agent-mail
- `codex-rs/core/src/session/agent_mail.rs`: accepted-agent-mail
- `codex-rs/state/src/runtime/agent_mail.rs`: accepted-agent-mail
- `codex-rs/thread-store/src/live_thread/agent_mail.rs`: accepted-agent-mail
- `codex-rs/thread-store/src/local/agent_mail.rs`: accepted-agent-mail
- `codex-rs/app-server/src/request_processors/agent_mail.rs`: accepted-agent-mail
- `codex-rs/core/src/context/compaction_continuation.rs`: compaction-is-not-input


- `codex-rs/tui/src/local_shell.rs`: client-only-shell
- `codex-rs/tui/src/app/local_shell.rs`: client-only-shell
- `codex-rs/tui/src/bottom_pane/chat_composer/local_shell.rs`: client-only-shell
- `codex-rs/tui/src/chatwidget/local_shell.rs`: client-only-shell



- `codex-rs/tui/src/app/turn_submissions.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/turn_submission_controller.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_operation_requests.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_realtime_operations.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_control_operations.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_navigation_operations.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_navigation_cleanup.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_detach_operations.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_directory_navigation.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_fork_navigation.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_navigation_permissions.rs`: durable-approvals-and-queues
- `codex-rs/core/src/agent/control/farm_reservation.rs`: server-hosted-agents

- `codex-rs/tui/src/streaming/immutable_rows.rs`: fast-ui-thread

- `codex-rs/tui/src/streaming/completion.rs`: fast-ui-thread

- `codex-rs/tui/src/chatwidget/stream_preparation.rs`: fast-ui-thread

- `codex-rs/tui/src/transcript_view/layout_rewrap.rs`: fast-ui-thread

- `codex-rs/tui/src/transcript_view/live_preparation.rs`: fast-ui-thread

- `codex-rs/tui/src/transcript_view/preparation.rs`: fast-ui-thread

- `codex-rs/tui/src/tui/background_preparation.rs`: fast-ui-thread

- `codex-rs/tui/src/terminal_probe/bracketed_paste.rs`: fast-ui-thread
