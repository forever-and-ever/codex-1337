#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.14"
# dependencies = []
# ///
"""Gate fork-owned Rust files on the house lint policy in codex-rs/fork-lints.toml.

A file is fork-owned when the latest merged or incoming upstream release lacks its path;
upstream files keep upstream's [workspace.lints]. Clippy runs with every lint capped at
warn, so upstream code cannot stop the build. Any diagnostic whose primary span lies
in a fork-owned file fails the gate.
Exit status: 0 clean, 1 findings, 2 the build itself failed.
"""

import argparse
import json
import subprocess
import sys
import tomllib
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKSPACE = ROOT / "codex-rs"
POLICY = WORKSPACE / "fork-lints.toml"
TOOL_PREFIXES = {"rust": "", "clippy": "clippy::"}


@dataclass(frozen=True, slots=True, order=True)
class Lint:
    priority: int
    name: str
    level: str

    @property
    def flag(self) -> str:
        return f"--{self.level}={self.name}"


@dataclass(frozen=True, slots=True, order=True)
class Finding:
    path: str
    line: int
    column: int
    code: str
    message: str


@dataclass(frozen=True, slots=True)
class Ownership:
    tag: str
    upstream: frozenset[str]
    only: tuple[str, ...]

    def claims(self, path: str) -> bool:
        return path not in self.upstream and (
            not self.only or path.startswith(self.only)
        )


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout


def ownership(only: list[str]) -> Ownership:
    # During an upstream merge, incoming release files already belong to upstream.
    merge_head = ROOT / git("rev-parse", "--git-path", "MERGE_HEAD").strip()
    incoming = merge_head.read_text().split() if merge_head.is_file() else []
    merged = [arg for commit in ("HEAD", *incoming) for arg in ("--merged", commit)]
    tag = git(
        "tag", *merged, "--sort=version:refname", "--list", "rust-v[0-9]*"
    ).split()[-1]
    upstream = frozenset(
        git("ls-tree", "-r", "-z", "--name-only", tag, "--", "codex-rs").split("\0")[
            :-1
        ]
    )
    return Ownership(tag, upstream, tuple(only))


def lint_flags() -> list[str]:
    tables = tomllib.loads(POLICY.read_text(encoding="utf-8"))["lints"]
    lints = sorted(
        Lint(0, prefix + name, spec)
        if isinstance(spec, str)
        else Lint(spec["priority"], prefix + name, spec["level"])
        for tool, prefix in TOOL_PREFIXES.items()
        for name, spec in tables[tool].items()
    )
    return [lint.flag for lint in lints]


def clippy(
    scope: list[str], owner: Ownership
) -> tuple[int, dict[Finding, str], list[str]]:
    command = [
        "cargo",
        "clippy",
        "--all-targets",
        "--keep-going",
        "--message-format=json",
        *scope,
    ]
    command += ["--", "--cap-lints=warn", *lint_flags()]
    found: dict[Finding, str] = {}
    errors: list[str] = []
    with subprocess.Popen(
        command, cwd=WORKSPACE, stdout=subprocess.PIPE, text=True
    ) as cargo:
        assert cargo.stdout is not None
        for line in cargo.stdout:
            record = json.loads(line)
            if record.get("reason") != "compiler-message":
                continue
            message = record["message"]
            if message["level"] == "error":
                errors.append(message["rendered"])
            span = next((span for span in message["spans"] if span["is_primary"]), None)
            if span is None or message["level"] not in ("warning", "error"):
                continue
            path = (WORKSPACE / span["file_name"]).resolve()
            if not path.is_relative_to(ROOT):
                continue
            relative = path.relative_to(ROOT).as_posix()
            if not owner.claims(relative):
                continue
            code = (message["code"] or {}).get("code", message["level"])
            finding = Finding(
                relative,
                span["line_start"],
                span["column_start"],
                code,
                message["message"],
            )
            found.setdefault(finding, message["rendered"])
    return cargo.returncode, found, errors


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "-p",
        "--package",
        action="append",
        default=[],
        help="lint these packages, not the workspace",
    )
    parser.add_argument(
        "--only",
        action="append",
        default=[],
        help="report only fork files under this repo path prefix",
    )
    args = parser.parse_args()
    owner = ownership(args.only)
    scope = [arg for package in args.package for arg in ("-p", package)] or [
        "--workspace"
    ]
    status, found, errors = clippy(scope, owner)
    for finding in sorted(found):
        print(found[finding], end="")
    if found:
        files = {finding.path for finding in found}
        print(
            f"check_fork_lints: {len(found)} findings in {len(files)} fork-owned files against {owner.tag}"
        )
        for code, count in Counter(finding.code for finding in found).most_common():
            print(f"{count:6} {code}")
    if status:
        print("".join(errors), end="", file=sys.stderr)
        print(f"check_fork_lints: cargo clippy exited {status}", file=sys.stderr)
        return 2
    if not found:
        print(f"check_fork_lints: fork-owned files are clean against {owner.tag}")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
