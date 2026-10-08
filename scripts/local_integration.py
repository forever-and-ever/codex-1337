"""Own the local Codex daemon integration independently of any frontend."""

import argparse
import os
import shlex
import subprocess
import tempfile
from pathlib import Path

UNITS = (
    "codex-app-server.service",
    "codex-app-server-refresh.service",
    "codex-app-server-refresh.path",
)
STAGING_UNIT = "codex-app-server-staging.service"
STAGING_LAUNCHER = "codex-staging"


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


def write_unit(unit_dir: Path, name: str, contents: str) -> None:
    with tempfile.NamedTemporaryFile(mode="w", dir=unit_dir, delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(contents)
            output.flush()
            os.fsync(output.fileno())
            temporary.replace(unit_dir / name)
        finally:
            temporary.unlink(missing_ok=True)


def launcher_marker(channel: str) -> bytes:
    label = "launcher" if channel == "current" else f"{channel} launcher"
    return f"# codex-1337 {label}\n".encode()


def launcher_contents(distributions: Path, channel: str) -> str:
    command = shlex.quote(str(distributions / channel / "bin/codex"))
    marker = launcher_marker(channel).decode()
    return f'#!/bin/sh\n{marker}exec {command} "$@"\n'


def check_launcher_ownership(distributions: Path, launcher: Path, channel: str) -> None:
    if not distributions.is_absolute() or not launcher.is_absolute():
        raise ValueError("launcher and distributions paths must be absolute")
    if launcher.is_symlink():
        if launcher.readlink() != distributions / channel / "bin/codex":
            raise FileExistsError(
                f"refusing to replace an unowned launcher: {launcher}"
            )
    elif launcher.exists():
        with launcher.open("rb") as existing:
            existing.readline()
            if existing.readline() != launcher_marker(channel):
                raise FileExistsError(
                    f"refusing to replace an unowned launcher: {launcher}"
                )


def install_launcher(distributions: Path, launcher: Path, channel: str) -> None:
    check_launcher_ownership(distributions, launcher, channel)
    contents = launcher_contents(distributions, channel)
    with tempfile.NamedTemporaryFile(
        mode="w", dir=launcher.parent, delete=False
    ) as output:
        temporary = Path(output.name)
        try:
            output.write(contents)
            output.flush()
            os.fsync(output.fileno())
            temporary.chmod(0o755)
            temporary.replace(launcher)
        finally:
            temporary.unlink(missing_ok=True)


def integrate_staging(action: str, install_root: Path) -> None:
    """A trial daemon beside production: its own launcher and unit, nothing else."""
    unit_dir = xdg("XDG_CONFIG_HOME", ".config") / "systemd/user"
    distributions = xdg("XDG_DATA_HOME", ".local/share") / "codex-1337/distributions"
    launcher = install_root / "bin" / STAGING_LAUNCHER
    if action == "uninstall-staging":
        if (unit_dir / STAGING_UNIT).exists():
            systemctl("disable", "--now", STAGING_UNIT)
            (unit_dir / STAGING_UNIT).unlink()
            systemctl("daemon-reload")
        check_launcher_ownership(distributions, launcher, "staging")
        launcher.unlink(missing_ok=True)
        return
    install_launcher(distributions, launcher, "staging")
    unit_dir.mkdir(parents=True, exist_ok=True)
    template = (Path(__file__).parent / "systemd" / STAGING_UNIT).read_text()
    contents = template.replace(
        "@CODEX@", unit_path(distributions / "staging/bin/codex")
    )
    write_unit(unit_dir, STAGING_UNIT, contents)
    systemctl("daemon-reload")
    systemctl("enable", STAGING_UNIT)
    # A trial build replaces its predecessor at once; staging turns may be cut short.
    systemctl("restart", STAGING_UNIT)


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
        write_unit(unit_dir, name, contents)
    systemctl("daemon-reload")
    systemctl("enable", "--now", UNITS[0], UNITS[2])
    # Do not restart: adoption waits for idle without closing admission to new work.
    systemctl("start", UNITS[1])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "action",
        choices=(
            "install",
            "install-launcher",
            "check-launcher-ownership",
            "uninstall",
            "install-staging",
            "uninstall-staging",
        ),
    )
    parser.add_argument("install_root", type=Path)
    parser.add_argument("launcher", nargs="?", type=Path)
    args = parser.parse_args()
    if not args.install_root.is_absolute():
        parser.error("install_root must be absolute")
    if args.action in ("install-launcher", "check-launcher-ownership"):
        if args.launcher is None:
            parser.error("launcher action requires a destination")
        if args.action == "check-launcher-ownership":
            check_launcher_ownership(args.install_root, args.launcher, "current")
        else:
            install_launcher(args.install_root, args.launcher, "current")
    elif args.action in ("install-staging", "uninstall-staging"):
        integrate_staging(args.action, args.install_root)
    else:
        integrate(args.action, args.install_root)
