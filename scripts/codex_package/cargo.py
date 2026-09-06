"""Cargo builds for source-built Codex package artifacts."""

import json
import os
import subprocess
from dataclasses import dataclass
from pathlib import Path

from .targets import REPO_ROOT, PackageVariant, TargetSpec
from .v8 import resolve_codex_v8_cargo_env

CODEX_RS_ROOT = REPO_ROOT / "codex-rs"


@dataclass(frozen=True)
class SourceBuildOutputs:
    entrypoint_bin: Path
    code_mode_host_bin: Path
    bwrap_bin: Path | None
    codex_command_runner_bin: Path | None
    codex_windows_sandbox_setup_bin: Path | None


def build_source_binaries(
    spec: TargetSpec,
    variant: PackageVariant,
    *,
    cargo: str,
    profile: str,
    entrypoint_bin: Path | None,
    code_mode_host_bin: Path | None,
    bwrap_bin: Path | None,
    codex_command_runner_bin: Path | None,
    codex_windows_sandbox_setup_bin: Path | None,
) -> SourceBuildOutputs:
    validate_prebuilt_resource_inputs(
        spec,
        bwrap_bin=bwrap_bin,
        codex_command_runner_bin=codex_command_runner_bin,
        codex_windows_sandbox_setup_bin=codex_windows_sandbox_setup_bin,
    )
    binaries = source_binaries_for_target(
        spec,
        variant,
        build_entrypoint=entrypoint_bin is None,
        build_code_mode_host=code_mode_host_bin is None,
        build_bwrap=spec.is_linux and bwrap_bin is None,
        build_codex_command_runner=spec.is_windows and codex_command_runner_bin is None,
        build_codex_windows_sandbox_setup=spec.is_windows
        and codex_windows_sandbox_setup_bin is None,
    )
    built_paths: dict[str, Path] = {}
    if binaries:
        cmd = [
            cargo,
            "build",
            "--target",
            spec.target,
            "--profile",
            profile,
            "--message-format=json-render-diagnostics",
        ]
        for binary in binaries:
            cmd.extend(["--bin", binary])

        cargo_env = None
        if entrypoint_bin is None or code_mode_host_bin is None:
            codex_v8_env = resolve_codex_v8_cargo_env(spec)
            if codex_v8_env:
                cargo_env = {**os.environ, **codex_v8_env}

        print("+", " ".join(cmd))
        result = subprocess.run(
            cmd,
            cwd=CODEX_RS_ROOT,
            check=True,
            env=cargo_env,
            stdout=subprocess.PIPE,
            text=True,
        )
        for line in result.stdout.splitlines():
            artifact = json.loads(line)
            if artifact.get("reason") == "compiler-artifact" and artifact.get(
                "executable"
            ):
                built_paths[artifact["target"]["name"]] = Path(artifact["executable"])
        for binary in binaries:
            if binary not in built_paths:
                raise RuntimeError(f"Cargo omitted executable artifact for {binary}")

    def artifact(name: str, explicit: Path | None) -> Path:
        return (explicit if explicit is not None else built_paths[name]).resolve()

    outputs = SourceBuildOutputs(
        entrypoint_bin=artifact(variant.cargo_bin, entrypoint_bin),
        code_mode_host_bin=artifact("codex-code-mode-host", code_mode_host_bin),
        bwrap_bin=artifact("bwrap", bwrap_bin) if spec.is_linux else None,
        codex_command_runner_bin=artifact(
            "codex-command-runner", codex_command_runner_bin
        )
        if spec.is_windows
        else None,
        codex_windows_sandbox_setup_bin=artifact(
            "codex-windows-sandbox-setup", codex_windows_sandbox_setup_bin
        )
        if spec.is_windows
        else None,
    )
    validate_source_outputs(outputs)
    return outputs


def source_binaries_for_target(
    spec: TargetSpec,
    variant: PackageVariant,
    *,
    build_entrypoint: bool,
    build_code_mode_host: bool,
    build_bwrap: bool,
    build_codex_command_runner: bool,
    build_codex_windows_sandbox_setup: bool,
) -> list[str]:
    binaries = []
    if build_entrypoint:
        binaries.append(variant.cargo_bin)
    if build_code_mode_host:
        binaries.append("codex-code-mode-host")
    if build_bwrap:
        binaries.append("bwrap")
    if build_codex_command_runner:
        binaries.append("codex-command-runner")
    if build_codex_windows_sandbox_setup:
        binaries.append("codex-windows-sandbox-setup")
    return binaries


def validate_prebuilt_resource_inputs(
    spec: TargetSpec,
    *,
    bwrap_bin: Path | None,
    codex_command_runner_bin: Path | None,
    codex_windows_sandbox_setup_bin: Path | None,
) -> None:
    if bwrap_bin is not None and not spec.is_linux:
        raise RuntimeError("--bwrap-bin is only supported for Linux targets.")
    if codex_command_runner_bin is not None and not spec.is_windows:
        raise RuntimeError(
            "--codex-command-runner-bin is only supported for Windows targets."
        )
    if codex_windows_sandbox_setup_bin is not None and not spec.is_windows:
        raise RuntimeError(
            "--codex-windows-sandbox-setup-bin is only supported for Windows targets."
        )


def validate_source_outputs(outputs: SourceBuildOutputs) -> None:
    for path in [
        outputs.entrypoint_bin,
        outputs.code_mode_host_bin,
        outputs.bwrap_bin,
        outputs.codex_command_runner_bin,
        outputs.codex_windows_sandbox_setup_bin,
    ]:
        if path is not None and not path.is_file():
            raise RuntimeError(f"cargo build did not produce expected binary: {path}")
