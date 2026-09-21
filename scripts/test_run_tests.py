"""Exercise scratch ownership across concurrent runs, failures, and cancellation."""

import json
import os
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


LAUNCHER = Path(__file__).with_name("run_tests.py")
PROBE = """import json, os, signal, sys
from pathlib import Path

root = Path(os.environ['TMPDIR'])
leak = root / 'codex-state-runtime-test-leak'
leak.mkdir()
(leak / 'db.sqlite').write_bytes(b'fixture')
(leak / 'outside').symlink_to(root.parent / 'not-test-scratch')
leak.chmod(0o500)
def stop(sig, _):
    # Cleanup must follow, not race, the test process shutting down.
    assert (leak / 'db.sqlite').read_bytes() == b'fixture'
    sys.exit(128 + sig)
for sig in (signal.SIGTERM, signal.SIGINT):
    signal.signal(sig, stop)
print(json.dumps([str(root), sys.argv[1:], os.environ['TEMP'], os.environ['TMP']]), flush=True)
action = sys.stdin.readline().strip()
if action == 'kill':
    os.kill(os.getpid(), signal.SIGKILL)
sys.exit(int(action))
"""


@unittest.skipUnless(os.name == "posix", "Unix test-runner contract")
class TestScratchLifecycle(unittest.TestCase):
    def test_runs_clean_only_their_own_scratch_after_exit_or_cancel(self) -> None:
        with tempfile.TemporaryDirectory(dir="/var/tmp") as fixture:
            root = Path(fixture)
            cargo = root / "cargo"
            cargo.write_text(f"#!{sys.executable}\n{PROBE}")
            cargo.chmod(0o700)
            sentinel = root / "not-test-scratch"
            sentinel.write_text("keep")
            environment = os.environ | {
                "PATH": f"{root}:{os.environ['PATH']}",
                "CODEX_TEST_TMP_ROOT": str(root),
                "TMPDIR": "/tmp",  # Ambient tmpfs must not win over run ownership.
            }
            command = [sys.executable, str(LAUNCHER), "-E", "test(a b)"]
            # Keep one run alive while peers finish/fail/receive cancellation.
            with subprocess.Popen(
                command,
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            ) as survivor:
                assert survivor.stdout is not None
                retained = Path(json.loads(survivor.stdout.readline())[0])
                for status, interruption in [
                    (0, None),
                    (37, None),
                    (137, None),
                    (143, signal.SIGTERM),
                    (130, signal.SIGINT),
                ]:
                    with (
                        self.subTest(status=status),
                        subprocess.Popen(
                            command,
                            env=environment,
                            stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE,
                            text=True,
                        ) as run,
                    ):
                        assert run.stdout is not None
                        scratch, args, temp, tmp = json.loads(run.stdout.readline())
                        self.assertEqual(
                            args,
                            ["nextest", "run", "--no-fail-fast", "-E", "test(a b)"],
                        )
                        self.assertEqual((temp, tmp), (scratch, scratch))
                        self.assertNotEqual(Path(scratch), retained)
                        if interruption is not None:
                            run.send_signal(interruption)
                            run.wait(timeout=10)
                            _, errors = run.communicate(timeout=10)
                        else:
                            action = "kill" if status == 137 else str(status)
                            _, errors = run.communicate(f"{action}\n", timeout=10)
                        self.assertEqual((run.returncode, errors), (status, ""))
                        self.assertFalse(Path(scratch).exists())
                        self.assertTrue(retained.is_dir())
                        self.assertEqual(sentinel.read_text(), "keep")
                _, errors = survivor.communicate("0\n", timeout=10)
                self.assertEqual((survivor.returncode, errors), (0, ""))
                self.assertFalse(retained.exists())


if __name__ == "__main__":
    unittest.main()
