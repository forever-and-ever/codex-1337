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
)
CUT_SNAPSHOTS = (
    "__slash_pets",
    "__ambient_pet",
    "__footer_status_line_enabled_mode_and_ide_context_right",
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
    r"|\bexternal_agent_config_processor\b|codex_external_agent_migration::",
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
    workspace = tomllib.loads((ROOT / "codex-rs/Cargo.toml").read_text())["workspace"]
    if "external-agent-migration" in workspace["members"]:
        failures.append("import crate restored to workspace")
    if "external-agent-migration" not in workspace.get("exclude", []):
        failures.append("import crate must stay excluded from workspace")
    if "codex-external-agent-migration" in (ROOT / "codex-rs/Cargo.lock").read_text():
        failures.append("import crate restored to Cargo.lock")
    features = (ROOT / "codex-rs/features/src/lib.rs").read_text()
    for feature in ("ExternalAgentMemoryImport", "ExternalMigration"):
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
