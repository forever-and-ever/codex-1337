"""Supply the packaging V8 artifacts to ordinary Cargo and language-server builds."""

import argparse
import json
import os
import subprocess
import sys

from .targets import TARGET_SPECS
from .v8 import needs_codex_v8_artifacts, resolve_codex_v8_cargo_env


def main() -> None:
    cargo, *args = sys.argv[1:]
    if not needs_codex_v8_artifacts(os.environ):
        os.execvpe(cargo, [cargo, *args], os.environ)
    toolchain = args[:1] if args and args[0].startswith("+") else []
    options = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
    options.add_argument("--target", action="append")
    options.add_argument("--config", action="append", default=[])
    options.add_argument("-C")
    parsed, _ = options.parse_known_args(
        args[: args.index("--")] if "--" in args else args
    )

    query = [cargo, *toolchain]
    for config in parsed.config:
        query.extend(["--config", config])
    if parsed.C is not None:
        query.extend(["-C", parsed.C])
    query.extend(["-Zunstable-options", "config", "get", "build", "--format=json"])
    config = json.loads(subprocess.check_output(query, text=True))
    targets = parsed.target or config["build"].get("target")
    if not targets:
        targets = subprocess.check_output(
            ["rustc", *toolchain, "--print", "host-tuple"], text=True
        ).strip()
    if isinstance(targets, str):
        targets = [targets]
    if len(set(targets)) != 1 or targets[0] not in TARGET_SPECS:
        raise RuntimeError(
            f"V8 artifacts require one supported Cargo target: {targets}"
        )
    environment = os.environ | resolve_codex_v8_cargo_env(TARGET_SPECS[targets[0]])
    os.execvpe(cargo, [cargo, *args], environment)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(f"Cargo V8 environment: {error}")
