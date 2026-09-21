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
    root = Path(os.environ.get("CODEX_TEST_TMP_ROOT", "/var/tmp")).resolve()
    root.mkdir(parents=True, exist_ok=True)
    if sys.platform == "linux":
        filesystem = subprocess.check_output(
            ["stat", "-f", "-c", "%T", "--", str(root)], text=True
        ).strip()
        if filesystem in {"tmpfs", "ramfs"}:
            sys.exit("CODEX_TEST_TMP_ROOT must be disk-backed, not tmpfs/ramfs")

    with tempfile.TemporaryDirectory(prefix="codex-test-", dir=root) as scratch:
        environment = os.environ | dict.fromkeys(("TMPDIR", "TMP", "TEMP"), scratch)
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
