"""Local distribution lifecycle and shared artifact-cache contracts."""

import hashlib
import os
import struct
import subprocess
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

import local_integration
import tui_revision
from codex_package import v8
from codex_package.targets import TARGET_SPECS


class LocalInstallTest(unittest.TestCase):
    def test_bundle_pruning_requires_host_visibility_and_preserves_active_releases(
        self,
    ) -> None:
        # PID 1 can belong to the sandbox, and non-dumpable same-user processes
        # hide their executables. Neither is evidence of absence, but only a
        # process named like a shipped executable can be an older TUI.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            distributions, proc = root / "distributions", root / "proc"
            for name in ("current-release", "previous", "live", "unused", "unowned"):
                bundle = distributions / name
                (bundle / "bin").mkdir(parents=True)
                (bundle / "bin/codex").touch()
                if name != "unowned":
                    (bundle / "distribution").touch()
            (distributions / "current").symlink_to("current-release")
            (distributions / "alias").symlink_to("unused")
            owner = "Uid:" + f"\t{os.getuid()}" * 4 + "\n"
            for entry, name, nspid in (
                ("self", "python3", os.getpid()),
                ("7", "codex", 7),
                ("9", "ssh-agent", 9),
            ):
                (proc / entry).mkdir(parents=True)
                (proc / entry / "status").write_text(
                    f"Name:\t{name}\n{owner}NSpid:\t{nspid}\n"
                )
            (proc / "7/exe").symlink_to(distributions / "live/bin/codex")
            device = proc.stat().st_dev
            mountinfo = (
                f"1 0 {os.major(device)}:{os.minor(device)} / /proc rw - proc proc rw\n"
            )
            (proc / "self/mountinfo").write_text(mountinfo)

            with (
                patch.object(local_integration, "Path", return_value=proc),
                patch.object(local_integration.socket, "socket") as socket,
                patch.object(local_integration.sys, "stderr"),
            ):
                manager = socket.return_value.__enter__.return_value
                credentials = manager.getsockopt
                for pid, uid in ((0, os.getuid()), (7, os.getuid() + 1)):
                    credentials.return_value = struct.pack("3i", pid, uid, 0)
                    local_integration.prune_distributions(distributions, "previous")
                    self.assertTrue((distributions / "unused").exists())
                credentials.return_value = struct.pack("3i", 7, os.getuid(), 0)

                status = (proc / "7/status").read_text()
                (proc / "7/status").write_text(
                    status.replace("NSpid:\t7", "NSpid:\t1\t7")
                )
                local_integration.prune_distributions(distributions, "previous")
                self.assertTrue((distributions / "unused").exists())
                (proc / "7/status").write_text(status)

                for restricted in (
                    mountinfo.rstrip() + ",hidepid=4\n",
                    mountinfo + "2 1 0:1 / /proc/8 rw - tmpfs tmpfs rw\n",
                    "",
                ):
                    (proc / "self/mountinfo").write_text(restricted)
                    local_integration.prune_distributions(distributions, "previous")
                    self.assertTrue((distributions / "unused").exists())
                (proc / "self/mountinfo").write_text(mountinfo)

                with patch.object(Path, "readlink", side_effect=PermissionError):
                    local_integration.prune_distributions(distributions, "previous")
                    self.assertTrue((distributions / "unused").exists())
                manager.connect.side_effect = PermissionError
                local_integration.prune_distributions(distributions, "previous")
                self.assertTrue((distributions / "unused").exists())
                manager.connect.side_effect = None

                # A process disappearing during enumeration is not hidden, and a
                # non-dumpable agent is no shipped executable.
                (proc / "8").mkdir()
                readlink = Path.readlink

                def hide_agent(path: Path) -> Path:
                    if path == proc / "9/exe":
                        raise PermissionError(path)
                    return readlink(path)

                with patch.object(
                    Path, "readlink", autospec=True, side_effect=hide_agent
                ):
                    local_integration.prune_distributions(distributions, "previous")
                manager.connect.assert_called_with(
                    f"/run/user/{os.getuid()}/systemd/private"
                )
            self.assertFalse((distributions / "unused").exists())
            self.assertEqual(
                {entry.name for entry in distributions.iterdir()},
                {"current-release", "previous", "live", "unowned", "current", "alias"},
            )

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

    def test_daemon_lifecycle_preserves_data_and_drains_owned_services(self) -> None:
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
                production_unit = (units / local_integration.UNITS[0]).read_text()
                self.assertIn(
                    str(prefix / "bin/codex"),
                    production_unit,
                )
                self.assertIn(
                    "--linux-sandbox-pid-namespace inherit",
                    production_unit,
                )
                # Systemd supervision does not enable the server's durable owners.
                self.assertIn("--managed-daemon", production_unit)
                systemctl.reset_mock()
                local_integration.integrate("install", prefix)
                self.assertEqual(systemctl.call_args_list[0].args, ("daemon-reload",))
                systemctl.reset_mock()
                local_integration.integrate_staging("install-staging", prefix)
                self.assertEqual(
                    [call.args for call in systemctl.call_args_list],
                    [
                        ("daemon-reload",),
                        ("enable", local_integration.STAGING_UNIT),
                        ("restart", local_integration.STAGING_UNIT),
                    ],
                )
                staging_unit = (units / local_integration.STAGING_UNIT).read_text()
                self.assertIn("--managed-daemon", staging_unit)
                self.assertIn(
                    str(data / "codex-1337/staging/distributions/current/bin/codex"),
                    staging_unit,
                )
                self.assertEqual(
                    (units / local_integration.UNITS[0]).read_text(), production_unit
                )
                local_integration.integrate_staging("uninstall-staging", prefix)
                systemctl.reset_mock()
                # Clipboard ownership is a separate opt-in, not daemon deployment.
                local_integration.install_clipboard_keeper(prefix)
                systemctl.assert_not_called()
                keeper_unit = (units / "codex-clipboard-keeper.service").read_text()
                self.assertIn("Restart=no", keeper_unit)
                self.assertIn("x11-apps.target", keeper_unit)
                self.assertIn(
                    str(prefix / "libexec/codex-clipboard-keeper"), keeper_unit
                )
                self.assertEqual(
                    (prefix / "libexec/codex-clipboard-keeper").read_bytes(),
                    (Path(__file__).parent / "clipboard-keeper").read_bytes(),
                )
                # Daemon uninstallation must not remove a desktop-wide keeper.
                local_integration.integrate("uninstall", prefix)
                self.assertEqual(
                    [call.args for call in systemctl.call_args_list],
                    [
                        (
                            "disable",
                            "--now",
                            "codex-app-server-refresh.path",
                            "codex-app-server.service",
                        ),
                        ("daemon-reload",),
                    ],
                )
                self.assertEqual(
                    [unit.name for unit in units.iterdir()],
                    ["codex-clipboard-keeper.service"],
                )
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
                local_integration.install_launcher(
                    distributions, launcher, "production"
                )
            launcher.unlink()
            launcher.symlink_to(distributions / "current/bin/codex")
            local_integration.install_launcher(distributions, launcher, "production")
            local_integration.install_launcher(distributions, launcher, "production")
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
