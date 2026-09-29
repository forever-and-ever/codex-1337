"""Local distribution lifecycle and shared artifact-cache contracts."""

import hashlib
from concurrent.futures import ThreadPoolExecutor
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import local_integration
import tui_revision
from codex_package import v8
from codex_package.targets import TARGET_SPECS


class LocalInstallTest(unittest.TestCase):
    def test_tui_revisions_survive_reinstallation_and_serialize_publishers(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            state = root / "state"
            packages = []
            for index, contents in enumerate(
                (b"first", b"second", b"second", b"third")
            ):
                package = root / f"package-{index}"
                (package / "bin").mkdir(parents=True)
                (package / "bin/codex").write_bytes(contents)
                (package / "distribution").write_text(f"source_commit=commit-{index}\n")
                packages.append(package)
            self.assertEqual(tui_revision.stamp(packages[0], state), 0)
            with ThreadPoolExecutor(max_workers=3) as publishers:
                revisions = list(
                    publishers.map(
                        lambda package: tui_revision.stamp(package, state), packages[1:]
                    )
                )
            self.assertEqual(revisions[0], revisions[1])
            self.assertEqual(set(revisions), {1, 2})
            # Pruning an installed bundle must not erase its durable identity.
            (packages[0] / "distribution").write_text("source_commit=reinstalled\n")
            self.assertEqual(tui_revision.stamp(packages[0], state), 0)
            self.assertEqual(
                (packages[0] / "distribution").read_text(),
                "source_commit=reinstalled\ntui_revision=0\n",
            )

    def test_gui_retirement_preserves_data_and_drains_daemon(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            config, data, prefix = (root / name for name in ("config", "data", "local"))
            units = config / "systemd/user"
            units.mkdir(parents=True)
            (units / "codex-wrangler.service").touch()
            roster = data / "codex-wrangler/roster.json"
            roster.parent.mkdir(parents=True)
            roster.write_text("preserve")
            binary = prefix / "bin/codex-wrangler"
            binary.parent.mkdir(parents=True)
            binary.symlink_to(
                data / "codex-1337/distributions/current/bin/codex-wrangler"
            )
            with (
                patch.dict(
                    os.environ,
                    {"XDG_CONFIG_HOME": str(config), "XDG_DATA_HOME": str(data)},
                ),
                patch.object(local_integration, "systemctl") as systemctl,
            ):
                local_integration.integrate("install", prefix)
                self.assertEqual(
                    [call.args for call in systemctl.call_args_list],
                    [
                        ("disable", "--now", "codex-wrangler.service"),
                        ("daemon-reload",),
                        (
                            "enable",
                            "--now",
                            "codex-app-server.service",
                            "codex-app-server-refresh.path",
                        ),
                        ("start", "codex-app-server-refresh.service"),
                    ],
                )
                self.assertFalse(binary.is_symlink())
                self.assertIn(
                    str(prefix / "bin/codex"),
                    (units / local_integration.UNITS[0]).read_text(),
                )
                systemctl.reset_mock()
                local_integration.integrate("install", prefix)
                self.assertEqual(systemctl.call_args_list[0].args, ("daemon-reload",))
                local_integration.integrate("uninstall", prefix)
                self.assertEqual(list(units.iterdir()), [])
                self.assertEqual(roster.read_text(), "preserve")

    def test_launcher_preserves_arguments_and_refuses_unowned_files(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            distributions = root / "distributions"
            launcher = root / "bin/codex"
            launcher.parent.mkdir()
            executable = distributions / "current/bin/codex"
            executable.parent.mkdir(parents=True)
            executable.write_text("#!/bin/sh\nprintf '%s\\n' \"$@\"\n")
            executable.chmod(0o755)
            launcher.write_text("unowned\n")
            with self.assertRaises(FileExistsError):
                local_integration.install_launcher(distributions, launcher)
            launcher.unlink()
            launcher.symlink_to(distributions / "current/bin/codex")
            local_integration.install_launcher(distributions, launcher)
            local_integration.install_launcher(distributions, launcher)
            result = subprocess.run(
                [launcher, "two words", "*", ""],
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.stdout, "two words\n*\n\n")

    def test_v8_reuses_verified_artifacts_at_stable_paths(self) -> None:
        downloads: list[str] = []

        def download(url: str, destination: Path) -> None:
            downloads.append(url)
            destination.parent.mkdir(parents=True, exist_ok=True)
            if destination.suffix == ".sha256":
                stem = destination.name.removesuffix(".sha256")
                names = [
                    f"lib{stem}.a.gz",
                    f"src_binding_{stem.removeprefix('rusty_v8_')}.rs",
                ]
                digest = hashlib.sha256(b"artifact").hexdigest()
                destination.write_text("".join(f"{digest}  {name}\n" for name in names))
                trusted = (
                    v8.REPO_ROOT
                    / "third_party/v8/rusty_v8_test_release_manifests.sha256"
                )
                trusted.parent.mkdir(parents=True, exist_ok=True)
                trusted.write_text(
                    f"{hashlib.sha256(destination.read_bytes()).hexdigest()}  {destination.name}\n"
                )
            else:
                destination.write_bytes(b"artifact")

        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.object(v8, "REPO_ROOT", Path(temporary)),
            patch.dict(os.environ, {"XDG_CACHE_HOME": temporary}),
            patch.object(v8, "resolved_v8_crate_version", return_value="test"),
            patch.object(v8, "download_file", side_effect=download),
        ):
            spec = TARGET_SPECS["x86_64-unknown-linux-gnu"]
            first = v8.resolve_codex_v8_cargo_env(spec, environ={})
            second = v8.resolve_codex_v8_cargo_env(spec, environ={})
            self.assertEqual(first, second)
            self.assertEqual(
                len(downloads), 3
            )  # Pinned manifest and artifact pair are all reused.
            archive = Path(first["RUSTY_V8_ARCHIVE"])
            archive.write_bytes(b"corrupted")
            self.assertEqual(v8.resolve_codex_v8_cargo_env(spec, environ={}), first)
            self.assertEqual(archive.read_bytes(), b"artifact")


if __name__ == "__main__":
    unittest.main()
