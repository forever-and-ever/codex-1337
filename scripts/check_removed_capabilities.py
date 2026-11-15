#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Reapply capability cuts after upstream merges; --check never changes files."""

import argparse
import re
import subprocess
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CUT_PREFIXES = (
    "codex-rs/tui/src/pets/",
    "codex-rs/tui/src/app/pets.rs",
    "codex-rs/tui/src/chatwidget/pets",
    "codex-rs/tui/src/ide_context",
    "codex-rs/tui/src/chatwidget/ide_context.rs",
    "codex-rs/tui/src/external_agent_config_migration/",
    "codex-rs/tui/src/app_server_session/external_agent_config",
    "codex-rs/app-server/src/external_agent_migration/",
    "codex-rs/app-server/tests/suite/v2/external_agent",
    "codex-rs/state/src/runtime/external_agent_config_imports",
    "codex-rs/tui/src/bottom_pane/feedback_",
    "codex-rs/tui/src/app/feedback_upload.rs",
    "codex-rs/app-server/src/request_processors/feedback_",
    "codex-rs/app-server/src/log_write_warning",
    "codex-rs/app-server/tests/suite/v2/feedback.rs",
    "codex-rs/feedback/src/archive",
    "codex-rs/feedback/src/daemon_logs",
    "codex-rs/feedback/src/feedback_",
    "codex-rs/feedback/src/guardian",
    "codex-rs/feedback/src/lib_tests.rs",
    "codex-rs/feedback/src/metadata_tests.rs",
    "codex-rs/feedback/src/report_upload.rs",
    "codex-rs/feedback/src/rollout_",
    "codex-rs/feedback/src/save_rollout_tests.rs",
    "codex-rs/feedback/src/upload.rs",
    "codex-rs/state/src/runtime/logs",
    "codex-rs/state/src/runtime/guardian_feedback",
    "codex-rs/state/src/model/log.rs",
    "codex-rs/state/src/log_db_",
    "codex-rs/state/src/log_write_telemetry_tests.rs",
    "codex-rs/core/src/guardian/feedback.rs",
    "codex-rs/core/src/feedback_config",
    "codex-rs/ext/guardian-reviewer/src/feedback",
    "codex-rs/cli/src/bin/logs_client.rs",
    "codex-rs/state/logs_migrations/",
    "codex-rs/state/src/runtime/reclamation",
    "codex-rs/file-system/src/local_logs.rs",
    "codex-rs/thread-store/src/local/compression_writer_tests.rs",
    "codex-rs/cloud-client/",
    "codex-rs/app-server-client/src/cloud.rs",
    "codex-rs/app-server-client/src/cloud_backend/",
    "codex-rs/app-server-client/src/cloud_tests.rs",
    "codex-rs/tui/src/app/cloud_switch_wait.rs",
    "codex-rs/tui/src/app_server_session/cloud_project",
    "codex-rs/tui/src/transcript_view/turn_details",
    "codex-rs/tui/src/transcript_image",
    "codex-rs/cloud-tasks/",
    "codex-rs/cloud-tasks-client/",
    "codex-rs/cloud-tasks-mock-client/",
    "codex-rs/cli/tests/cloud_auth.rs",
    "codex-rs/tui/src/public_widgets/",
)
CUT_SNAPSHOTS = (
    "__slash_pets",
    "__ambient_pet",
    "__footer_status_line_enabled_mode_and_ide_context_right",
    "__feedback_note_view__",
    "__feedback_view__",
    "__feedback_selection_popup",
    "__feedback_upload_consent_popup",
    "__feedback_good_result_consent_popup",
)
UNHOOKED = "codex-rs/external-agent-migration/"
PROTOCOL_COMPATIBILITY = (
    "codex-rs/analytics/",
    "codex-rs/app-server-protocol/",
    "codex-rs/app-server/src/message_processor.rs",
    "codex-rs/app-server/src/in_process.rs",
    "codex-rs/app-server/src/notification_media.rs",
    "codex-rs/app-server/tests/suite/v2/request_validation.rs",
    "codex-rs/tui/src/app/app_server_event_targets.rs",
    "codex-rs/tui/src/chatwidget/protocol.rs",
)
IMPORT_REFERENCE = re.compile(
    r"external_agent_(?:config|migration)|ExternalAgentConfig|externalAgentConfig",
    re.IGNORECASE,
)
CUT_REFERENCES = re.compile(
    r"\b(?:ambient_pet|tui_pet|pet_picker|pet_selection)\w*|\btextarea_right_reserve\b"
    r"|\b(?:TuiPetAnchor|PetSelected|PetDisabled|PetPreview\w*|ConfiguredPetLoaded)\b"
    r"|SlashCommand::(?:Pets|Ide|Import)\b|\b(?:mod pets|pets::)"
    r"|\b(?:IdeContextState|set_ide_context_active|maybe_apply_ide_context)\b"
    r"|(?:mod |crate::)ide_context\b"
    r"|\b(?:mod |pub use )external_agent_(?:config|migration)\w*"
    r"|\bexternal_agent_config_processor\b|codex_external_agent_migration::"
    r"|SlashCommand::Feedback\b|\bFeedbackRequestProcessor\b|\blog_db::start\b"
    r"|\bquery_feedback_logs\w*|\bguardian_review_failures\b|\brecord_guardian_review_failure\b"
    r"|\b(?:logs_db_path|with_local_logs_home|LocalLogsDir|SqliteReclamationWorker)\b"
    r"|\b(?:spawn_rollout_compression_worker|RolloutCompressionTrigger|persist_temp_file_noclobber)\b"
    r"|\b(?:materialize_rollout_for_append\w*|try_acquire_for_publication|feedback_rollout_path)\b"
    r"|\bcodex_cloud_client\b|\b(?:AppServerTarget|ThreadParamsMode)::Cloud\b"
    r"|\bcollapse_turn_details\b|\bmod turn_details\b"
    r"|\bcodex_cloud_tasks\w*|\bCloudTasksCli\b|(?:mod |crate::)public_widgets\b",
    re.IGNORECASE,
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    files = (
        subprocess.check_output(
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            cwd=ROOT,
        )
        .decode()
        .split("\0")
    )
    failures = []
    for name in sorted(set(files)):
        path = ROOT / name
        if not name or not path.is_file():
            continue
        if name.startswith(CUT_PREFIXES) or (
            "/snapshots/" in name and any(marker in name for marker in CUT_SNAPSHOTS)
        ):
            if args.check:
                failures.append(f"removed capability file: {name}")
            else:
                path.unlink()
                print(f"deleted {name}")
        elif name.startswith("codex-rs/") and not name.startswith(UNHOOKED):
            if path.suffix == ".rs":
                for number, line in enumerate(path.read_text().splitlines(), 1):
                    if match := CUT_REFERENCES.search(line):
                        failures.append(f"{name}:{number}: cut reference {match[0]}")
                    elif not name.startswith(PROTOCOL_COMPATIBILITY) and (
                        match := IMPORT_REFERENCE.search(line)
                    ):
                        failures.append(f"{name}:{number}: import reference {match[0]}")
            elif path.name == "Cargo.toml" and re.search(
                r"codex-external-agent-migration|path\s*=\s*[\"'][^\"']*external-agent-migration",
                path.read_text(),
            ):
                failures.append(f"{name}: import crate dependency restored")
            elif path.name == "Cargo.toml" and re.search(
                r"codex-cloud-(?:client|tasks)|path\s*=\s*[\"'][^\"']*/cloud-(?:client|tasks[\w-]*)[\"']",
                path.read_text(),
            ):
                failures.append(f"{name}: Cloud connection dependency restored")
    workspace = tomllib.loads((ROOT / "codex-rs/Cargo.toml").read_text())["workspace"]
    if "external-agent-migration" in workspace["members"]:
        failures.append("import crate restored to workspace")
    if "external-agent-migration" not in workspace.get("exclude", []):
        failures.append("import crate must stay excluded from workspace")
    lock = (ROOT / "codex-rs/Cargo.lock").read_text()
    if "codex-external-agent-migration" in lock:
        failures.append("import crate restored to Cargo.lock")
    for member in (
        "cloud-client",
        "cloud-tasks",
        "cloud-tasks-client",
        "cloud-tasks-mock-client",
    ):
        if member in workspace["members"]:
            failures.append(f"Codex Cloud crate restored to workspace: {member}")
    if re.search(r'^name = "codex-cloud-(?:client|tasks[\w-]*)"$', lock, re.M):
        failures.append("Codex Cloud crate restored to Cargo.lock")
    features = (ROOT / "codex-rs/features/src/lib.rs").read_text()
    for feature in (
        "ExternalAgentMemoryImport",
        "ExternalMigration",
        "LocalThreadStoreCompression",
    ):
        if not re.search(
            rf"id: Feature::{feature},[^}}]*stage: Stage::Removed,", features
        ):
            failures.append(f"Feature::{feature} must stay removed")
    for migration in (
        "0038_external_agent_config_imports.sql",
        "0044_external_agent_config_imports_provider_id.sql",
    ):
        if not (ROOT / "codex-rs/state/migrations" / migration).is_file():
            failures.append(f"applied migration must remain: {migration}")
    for failure in failures:
        print(failure)
    return bool(failures)


if __name__ == "__main__":
    raise SystemExit(main())
