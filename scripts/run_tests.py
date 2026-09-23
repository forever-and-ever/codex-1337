"""Unix nextest launcher: one disk-backed scratch tree per run, not per test process.

Some fixtures return unowned paths; killed test processes also
skip destructors. The runner therefore owns cleanup, including failed runs.
SIGKILL of this runner cannot be cleaned up, but any leftovers stay off tmpfs.
No other run's directories are inspected or removed.
"""

import os
import signal
import subprocess
import sys
import tempfile
from pathlib import Path
from types import FrameType


def main() -> int:
    full_workspace = not sys.argv[1:] or "--workspace" in sys.argv[1:]
    if full_workspace:
        # The fork publishes its own checked V8 archive; rusty_v8's upstream
        # archive for this version is absent. Keep scoped crate runs lean.
        os.environ.setdefault(
            "CODEX_REPO_ROOT", str(Path(__file__).resolve().parents[1])
        )
        from codex_package.targets import TARGET_SPECS
        from codex_package.v8 import resolve_codex_v8_cargo_env

        host = subprocess.check_output(
            ["rustc", "--print", "host-tuple"], text=True
        ).strip()
        v8_environment = resolve_codex_v8_cargo_env(TARGET_SPECS[host])

    root = Path(os.environ.get("CODEX_TEST_TMP_ROOT", "/var/tmp")).resolve()
    root.mkdir(parents=True, exist_ok=True)
    if sys.platform == "linux":
        filesystem = subprocess.check_output(
            ["stat", "-f", "-c", "%T", "--", str(root)], text=True
        ).strip()
        if filesystem in {"tmpfs", "ramfs"}:
            sys.exit("CODEX_TEST_TMP_ROOT must be disk-backed, not tmpfs/ramfs")

    with tempfile.TemporaryDirectory(prefix="codex_test_", dir=root) as scratch:
        environment = os.environ | dict.fromkeys(("TMPDIR", "TMP", "TEMP"), scratch)
        if full_workspace:
            environment.update(v8_environment)
        child: subprocess.Popen[bytes] | None = None
        pending: list[int] = []

        def forward(signum: int, _frame: FrameType | None) -> None:
            if child is None:
                pending.append(signum)
            elif child.poll() is None:
                try:
                    os.killpg(child.pid, signum)
                except ProcessLookupError:
                    pass  # The child exited between poll and signal delivery.

        previous = {
            sig: signal.signal(sig, forward)
            for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
        }
        try:
            # Separate groups let a signal to this wrapper reach the entire owned run.
            # Waiting precedes directory cleanup; live tests never lose their scratch.
            with subprocess.Popen(
                ["cargo", "nextest", "run", "--no-fail-fast", *sys.argv[1:]],
                env=environment,
                start_new_session=True,
            ) as child:
                for sig in pending:
                    forward(sig, None)
                status = child.wait()
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)
    return status if status >= 0 else 128 - status


if __name__ == "__main__":
    raise SystemExit(main())
