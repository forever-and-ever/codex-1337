"""Unix nextest launcher: one disk-backed scratch tree per run, not per test process.

Some fixtures return unowned paths; killed test processes also
skip destructors. The runner therefore owns cleanup, including failed runs.
SIGKILL of this runner cannot be cleaned up, but any leftovers stay off tmpfs.
No other run's directories are inspected or removed. Requires Python 3.12+.
"""

import os
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from types import FrameType


def reap_orphans() -> bool:
    """Reap only this Linux subreaper's descendants, before deleting their scratch."""
    deadline = time.monotonic() + 5
    gentle_until = time.monotonic() + 1
    signalled: set[tuple[int, int]] = set()
    children = Path(f"/proc/self/task/{os.getpid()}/children")
    while True:
        try:
            while os.waitpid(-1, os.WNOHANG)[0]:
                pass
        except ChildProcessError:
            return True
        now = time.monotonic()
        if now >= deadline:
            return False
        sig = signal.SIGTERM if now < gentle_until else signal.SIGKILL
        for pid in map(int, children.read_text().split()):
            if (pid, sig) not in signalled:
                # Only this process can reap its direct children. Their PIDs
                # cannot be reused between the ownership check and this signal.
                try:
                    os.kill(pid, sig)
                except ProcessLookupError:
                    pass
                signalled.add((pid, sig))
        time.sleep(0.02)


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
        # Detached fixture daemons otherwise escape to PID 1 when nextest kills
        # a timed-out test. Adoption retains ownership without scanning the host.
        import ctypes

        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
            raise OSError(ctypes.get_errno(), "cannot own test descendants")

    directory = tempfile.TemporaryDirectory(
        prefix="codex_test_", dir=root, delete=False
    )
    scratch = directory.name
    clean = True
    try:
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

                def reap_exited(_signum: int, _frame: FrameType | None) -> None:
                    # Adoption also transfers zombie-reaping responsibility during
                    # the run. Leave Cargo's exit status to its Popen owner.
                    children = Path(f"/proc/self/task/{os.getpid()}/children")
                    for pid in map(int, children.read_text().split()):
                        if pid != child.pid:
                            try:
                                os.waitpid(pid, os.WNOHANG)
                            except ChildProcessError:
                                pass

                if sys.platform == "linux":
                    # Install only after Popen owns its PID: a child exiting during
                    # construction must not be mistaken for an adopted fixture.
                    previous_child = signal.signal(signal.SIGCHLD, reap_exited)
                    reap_exited(signal.SIGCHLD, None)
                try:
                    for sig in pending:
                        forward(sig, None)
                    status = child.wait()
                finally:
                    if sys.platform == "linux":
                        signal.signal(signal.SIGCHLD, previous_child)
        finally:
            if sys.platform == "linux":
                clean = False
                clean = reap_orphans()
            for sig, handler in previous.items():
                signal.signal(sig, handler)
    finally:
        if clean:
            directory.cleanup()
        else:
            print(
                f"Test descendants did not exit; retained scratch: {scratch}",
                file=sys.stderr,
            )
    if not clean:
        return 1
    return status if status >= 0 else 128 - status


if __name__ == "__main__":
    raise SystemExit(main())
