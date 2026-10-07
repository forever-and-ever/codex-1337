#!/usr/bin/env python3
"""Fail when a dev-dependency changes the identity of a normal build unit.

Cargo identifies a compilation unit by its package, target, features, and the
identities of its dependencies. A dev-dependency that enables one more feature
of a shared package therefore gives that package, and every unit above it, a
second identity in test builds: `cargo build` and `cargo test` then compile the
same crates twice and keep two incremental caches for each. This compares the
unit graphs of both commands and names each package whose features diverge.
Raise such a feature in `codex-rs/feature-floor`, or stop the dev-dependency
from requesting it when raising it would change shipped behavior.
"""

import hashlib
import json
import subprocess
import sys
from pathlib import Path

WORKSPACE = Path(__file__).resolve().parents[1] / "codex-rs"

type Unit = dict
type Key = tuple[str, str, str, str | None]


def unit_graph(command: str, *arguments: str) -> list[Unit]:
    """An explicit target gives target units a platform and leaves host units without one."""
    host = subprocess.run(
        ["rustc", "--print", "host-tuple"],
        cwd=WORKSPACE,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    ).stdout.strip()
    completed = subprocess.run(
        [
            "cargo",
            command,
            *arguments,
            "--workspace",
            "--target",
            host,
            "--unit-graph",
            "-Zunstable-options",
        ],
        cwd=WORKSPACE,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return json.loads(completed.stdout)["units"]


def key(unit: Unit) -> Key:
    return unit["pkg_id"], unit["target"]["name"], unit["mode"], unit["platform"]


def identities(units: list[Unit]) -> list[str]:
    """Hash each unit with its features and, recursively, its dependencies.

    Profiles are excluded: `--workspace` requests proc-macro crates as root
    units with the dev profile, while every real build compiles them as host
    dependencies, so their profiles differ only in this probe.
    """
    memo: dict[int, str] = {}

    def identity(index: int) -> str:
        if index not in memo:
            unit = units[index]
            dependencies = sorted(
                identity(edge["index"]) for edge in unit["dependencies"]
            )
            payload = json.dumps([key(unit), sorted(unit["features"]), dependencies])
            memo[index] = hashlib.sha256(payload.encode()).hexdigest()
        return memo[index]

    return [identity(index) for index in range(len(units))]


def main() -> int:
    normal = unit_graph("build")
    test = unit_graph("test", "--no-run")
    test_identities = set(identities(test))
    by_key: dict[Key, list[Unit]] = {}
    for unit in test:
        by_key.setdefault(key(unit), []).append(unit)

    def divergence(unit: Unit, counterpart_features: set[str]) -> str:
        features = set(unit["features"])
        added = sorted(counterpart_features - features)
        removed = sorted(features - counterpart_features)
        package = unit["pkg_id"].rpartition("#")[2]
        return f"{package} {unit['target']['name']}: test builds add {added} and drop {removed}"

    reports: set[str] = set()
    for unit, identity in zip(normal, identities(normal), strict=True):
        variants = by_key.get(key(unit), [])
        if identity in test_identities or not variants:
            continue
        twin = next((v for v in variants if v["features"] == unit["features"]), None)
        if twin is None:
            reports.add(divergence(unit, set(variants[0]["features"])))
            continue
        twin_dependencies = {
            key(test[edge["index"]]): test[edge["index"]]
            for edge in twin["dependencies"]
        }
        for edge in unit["dependencies"]:
            dependency = normal[edge["index"]]
            other = twin_dependencies.get(key(dependency))
            if other is not None and other["features"] != dependency["features"]:
                reports.add(divergence(dependency, set(other["features"])))
    for report in sorted(reports):
        print(report)
    if reports:
        print(
            f"{len(reports)} packages diverge between normal and test builds",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
