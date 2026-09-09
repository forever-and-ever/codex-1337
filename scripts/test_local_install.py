"""Local distribution lifecycle and shared artifact-cache contracts."""

import hashlib
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import local_integration
from codex_package import v8
from codex_package.targets import TARGET_SPECS


class LocalInstallTest(unittest.TestCase):
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
            else:
                destination.write_bytes(b"artifact")

        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.dict(os.environ, {"XDG_CACHE_HOME": temporary}),
            patch.object(v8, "resolved_v8_crate_version", return_value="test"),
            patch.object(v8, "download_file", side_effect=download),
        ):
            spec = TARGET_SPECS["x86_64-unknown-linux-gnu"]
            first = v8.resolve_codex_v8_cargo_env(spec, environ={})
            second = v8.resolve_codex_v8_cargo_env(spec, environ={})
            self.assertEqual(first, second)
            self.assertEqual(
                len(downloads), 4
            )  # Two manifests; artifact pair fetched once.
            archive = Path(first["RUSTY_V8_ARCHIVE"])
            archive.write_bytes(b"corrupted")
            self.assertEqual(v8.resolve_codex_v8_cargo_env(spec, environ={}), first)
            self.assertEqual(archive.read_bytes(), b"artifact")


if __name__ == "__main__":
    unittest.main()
