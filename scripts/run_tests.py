"""Unix nextest launcher: one disk-backed scratch tree per run, not per test process.

Some fixtures return unowned paths; killed test processes also
skip destructors. The runner therefore owns cleanup, including failed runs.
SIGKILL of this runner cannot be cleaned up, but any leftovers stay off tmpfs.
No other run's directories are inspected or removed. Requires Python 3.12+.
"""

import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from types import FrameType


# Binaries that tests spawn from other packages, which cargo does not build for
# them: (package, binary, packages whose tests spawn it). Derived on Linux from
# the tests' fixture lookups; a test that spawns a new binary needs a row here.
FIXTURE_BINARIES = (
    (
        "codex-app-server",
        "codex-app-server",
        (
            "app_test_support",
            "codex-app-server",
            "codex-cli",
            "codex-tui",
        ),
    ),
    ("codex-app-server", "codex-app-server-test-notify-capture", ("codex-app-server",)),
    (
        "codex-app-server",
        "exec-server",
        (
            "app_test_support",
            "codex-app-server",
            "codex-cli",
            "codex-core",
            "codex-tui",
        ),
    ),
    ("codex-apply-patch", "apply_patch", ("codex-apply-patch",)),
    (
        "codex-bwrap",
        "bwrap",
        (
            "app_test_support",
            "codex-agent-extension",
            "codex-app-server",
            "codex-cli",
            "codex-core",
            "codex-exec",
            "codex-exec-server",
            "codex-guardian-v2",
            "codex-history-notes-extension",
            "codex-linux-sandbox",
            "codex-login",
            "codex-mcp-extension",
            "codex-memories-write",
            "codex-queue-extension",
            "codex-tui",
            "core_test_support",
        ),
    ),
    # Core suites use codex-exec's arg0 helpers, except the RMCP sandbox
    # integration, which drives the full codex sandbox CLI.
    (
        "codex-cli",
        "codex",
        (
            "app_test_support",
            "codex-app-server",
            "codex-cli",
            "codex-rmcp-client",
            "codex-core",
            "codex-tui",
        ),
    ),
    (
        "codex-code-mode-host",
        "codex-code-mode-host",
        (
            "app_test_support",
            "codex-agent-extension",
            "codex-app-server",
            "codex-cli",
            "codex-code-mode-host",
            "codex-core",
            "codex-exec",
            "codex-guardian-v2",
            "codex-history-notes-extension",
            "codex-login",
            "codex-mcp-extension",
            "codex-memories-write",
            "codex-queue-extension",
            "codex-tui",
            "core_test_support",
        ),
    ),
    (
        "codex-exec",
        "codex-exec",
        (
            "app_test_support",
            "codex-agent-extension",
            "codex-app-server",
            "codex-cli",
            "codex-core",
            "codex-exec",
            "codex-guardian-v2",
            "codex-history-notes-extension",
            "codex-login",
            "codex-mcp-extension",
            "codex-memories-write",
            "codex-queue-extension",
            "codex-tui",
            "core_test_support",
        ),
    ),
    ("codex-http-client", "custom_ca_probe", ("codex-http-client",)),
    (
        "codex-linux-sandbox",
        "codex-linux-sandbox",
        (
            "app_test_support",
            "codex-agent-extension",
            "codex-app-server",
            "codex-cli",
            "codex-core",
            "codex-exec",
            "codex-guardian-v2",
            "codex-history-notes-extension",
            "codex-linux-sandbox",
            "codex-login",
            "codex-mcp-extension",
            "codex-memories-write",
            "codex-queue-extension",
            "codex-tui",
            "core_test_support",
        ),
    ),
    ("codex-message-board-mcp", "codex-message-board-mcp", ("codex-core",)),
    (
        "codex-rmcp-client",
        "test_mcp_2026_discovery_stdio_server",
        ("codex-rmcp-client",),
    ),
    ("codex-rmcp-client", "test_mcp_2026_stdio_server", ("codex-rmcp-client",)),
    (
        "codex-rmcp-client",
        "test_stdio_server",
        (
            "app_test_support",
            "codex-agent-extension",
            "codex-app-server",
            "codex-cli",
            "codex-core",
            "codex-exec",
            "codex-guardian-v2",
            "codex-history-notes-extension",
            "codex-login",
            "codex-mcp-extension",
            "codex-memories-write",
            "codex-queue-extension",
            "codex-rmcp-client",
            "codex-tui",
            "core_test_support",
        ),
    ),
    (
        "codex-rmcp-client",
        "test_streamable_http_server",
        (
            "codex-core",
            "codex-rmcp-client",
        ),
    ),
)


# Fixtures that unit tests spawn. A --lib run builds only these: building a
# package's binary compiles its library again outside the test configuration.
UNIT_TEST_FIXTURES = (
    (
        "codex-code-mode-host",
        "codex-code-mode-host",
        ("codex-core", "codex-guardian-v2"),
    ),
)


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
    if sys.platform == "linux" and shutil.which("bwrap") is None:
        sys.exit("bwrap is required to isolate the test runtime directory")
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

    # Build only the fixtures that the selected packages' tests spawn; a selection
    # this parser does not understand builds every fixture.
    selected_packages: set[str] = set()
    all_fixtures = False
    arguments = iter(sys.argv[1:])
    for argument in arguments:
        if argument in {"-p", "--package"}:
            selected_packages.add(next(arguments, ""))
        elif argument.startswith("--package="):
            selected_packages.add(argument.removeprefix("--package="))
        elif argument.startswith("-p"):
            selected_packages.add(argument[2:])
        elif argument in {"--workspace", "--exclude"} or argument.startswith(
            "--exclude="
        ):
            all_fixtures = True
    if not selected_packages or any(
        re.fullmatch(r"[A-Za-z0-9_-]+", package) is None
        for package in selected_packages
    ):
        all_fixtures = True
    unit_tests_only = "--lib" in sys.argv[1:] and not any(
        argument.partition("=")[0]
        in {
            "--bin",
            "--bins",
            "--example",
            "--examples",
            "--test",
            "--tests",
            "--bench",
            "--benches",
            "--all-targets",
        }
        for argument in sys.argv[1:]
    )
    fixtures = [
        (package, binary)
        for package, binary, consumers in (
            UNIT_TEST_FIXTURES if unit_tests_only else FIXTURE_BINARIES
        )
        if all_fixtures or selected_packages.intersection(consumers)
    ]
    commands = []
    if fixtures:
        build = ["cargo", "build"]
        for package, binary in fixtures:
            build.extend(("-p", package, "--bin", binary))
        commands.append(build)
    # nextest rejects --no-fail-fast for a build-only run.
    fail_fast = [] if "--no-run" in sys.argv[1:] else ["--no-fail-fast"]
    commands.append(["cargo", "nextest", "run", *fail_fast, *sys.argv[1:]])

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
        # Tests must not see the developer's Codex home, so every run gets an empty one.
        environment["CODEX_HOME"] = tempfile.mkdtemp(dir=scratch)
        if sys.platform == "linux":
            private_runtime = Path(scratch) / "runtime"
            private_runtime.mkdir(mode=0o700)
            runtime = Path(f"/run/user/{os.geteuid()}/codex-1337")
            runtime.mkdir(mode=0o700, exist_ok=True)
            private_legacy_runtime = Path(scratch) / "legacy-runtime"
            private_legacy_runtime.mkdir(mode=0o700)
            legacy_runtime = Path(f"/tmp/codex-daemon-{os.geteuid()}")
            legacy_runtime.mkdir(mode=0o700, exist_ok=True)
            # Tests must not reach the live runtime directories, where the daemon and
            # reload host keep their sockets; each run gets private ones.
            commands[-1] = [
                "bwrap",
                "--dev-bind",
                "/",
                "/",
                "--bind",
                str(private_runtime),
                str(runtime),
                "--bind",
                str(private_legacy_runtime),
                str(legacy_runtime),
                "--die-with-parent",
                "--",
                *commands[-1],
            ]
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
            for command in commands:
                with subprocess.Popen(
                    command,
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
                        pending.clear()
                        status = child.wait()
                    finally:
                        if sys.platform == "linux":
                            signal.signal(signal.SIGCHLD, previous_child)
                child = None
                if status != 0:
                    break
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
