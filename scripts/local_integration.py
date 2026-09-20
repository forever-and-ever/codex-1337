"""Own the local Codex daemon integration independently of any frontend."""

import argparse
import os
import subprocess
import tempfile
from pathlib import Path

UNITS = (
    "codex-app-server.service",
    "codex-app-server-refresh.service",
    "codex-app-server-refresh.path",
)


def xdg(name: str, fallback: str) -> Path:
    configured = Path(os.environ.get(name, ""))
    return configured if configured.is_absolute() else Path.home() / fallback


def systemctl(*arguments: str) -> None:
    subprocess.run(["systemctl", "--user", *arguments], check=True)


def unit_path(path: Path) -> str:
    # Unit values are not shell syntax; protect specifiers and ExecStart expansion.
    value = str(path).replace("%", "%%")
    value = value.replace("\\", "\\\\").replace('"', '\\"')
    return '"' + value.replace("\n", "\\n").replace("\r", "\\r") + '"'


def integrate(action: str, install_root: Path) -> None:
    if not install_root.is_absolute() or any(c in str(install_root) for c in "\r\n"):
        raise ValueError("install_root must be absolute and contain no line breaks")
    unit_dir = xdg("XDG_CONFIG_HOME", ".config") / "systemd/user"
    data = xdg("XDG_DATA_HOME", ".local/share")
    legacy_unit = unit_dir / "codex-wrangler.service"
    if legacy_unit.exists():
        systemctl("disable", "--now", legacy_unit.name)
        legacy_unit.unlink()
    (unit_dir / "codex-wrangler.service.d/90-local.conf").unlink(missing_ok=True)
    (data / "applications/codex-wrangler.desktop").unlink(missing_ok=True)
    legacy_binary = install_root / "bin/codex-wrangler"
    if legacy_binary.is_symlink() and legacy_binary.readlink() == (
        data / "codex-1337/distributions/current/bin/codex-wrangler"
    ):
        legacy_binary.unlink()

    if action == "uninstall":
        systemctl("disable", "--now", UNITS[2], UNITS[0])
        for name in UNITS:
            (unit_dir / name).unlink(missing_ok=True)
        systemctl("daemon-reload")
        return

    unit_dir.mkdir(parents=True, exist_ok=True)
    templates = Path(__file__).parent / "systemd"
    for name in UNITS:
        contents = (
            (templates / name)
            .read_text()
            .replace("@CODEX@", unit_path(install_root / "bin/codex"))
            .replace("@CODEX_PATH@", str(install_root / "bin/codex").replace("%", "%%"))
        )
        with tempfile.NamedTemporaryFile(
            mode="w", dir=unit_dir, delete=False
        ) as output:
            temporary = Path(output.name)
            try:
                output.write(contents)
                output.flush()
                os.fsync(output.fileno())
                temporary.replace(unit_dir / name)
            finally:
                temporary.unlink(missing_ok=True)
    systemctl("daemon-reload")
    systemctl("enable", "--now", UNITS[0], UNITS[2])
    # Do not restart: adoption waits for idle without closing admission to new work.
    systemctl("start", UNITS[1])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("install", "uninstall"))
    parser.add_argument("install_root", type=Path)
    args = parser.parse_args()
    if not args.install_root.is_absolute():
        parser.error("install_root must be absolute")
    integrate(args.action, args.install_root)
