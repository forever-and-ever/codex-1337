"""Assign persistent, content-addressed revision numbers to installed TUI bundles."""

import argparse
import hashlib
import sqlite3
from pathlib import Path


def stamp(package: Path, state: Path) -> int:
    manifest = package / "distribution"
    provenance = manifest.read_text()
    source = next(
        line.removeprefix("source_commit=")
        for line in provenance.splitlines()
        if line.startswith("source_commit=")
    )
    with (package / "bin/codex").open("rb") as binary:
        digest = hashlib.file_digest(binary, "sha256").hexdigest()
    state.mkdir(parents=True, exist_ok=True)
    database = sqlite3.connect(state / "tui-builds.sqlite")
    try:
        with database:
            database.execute(
                "CREATE TABLE IF NOT EXISTS builds ("
                "revision INTEGER PRIMARY KEY, digest TEXT NOT NULL UNIQUE, "
                "source_commit TEXT NOT NULL)"
            )
            database.execute("BEGIN IMMEDIATE")
            row = database.execute(
                "SELECT revision FROM builds WHERE digest = ?", (digest,)
            ).fetchone()
            if row is None:
                row = database.execute(
                    "INSERT INTO builds SELECT COALESCE(MAX(revision) + 1, 0), ?, ? "
                    "FROM builds RETURNING revision",
                    (digest, source),
                ).fetchone()
            revision = row[0]
    finally:
        database.close()
    # Only an unpublished package is stamped; running clients keep their own manifest.
    fields = [
        line for line in provenance.splitlines() if not line.startswith("tui_revision=")
    ]
    manifest.write_text("\n".join([*fields, f"tui_revision={revision}", ""]))
    return revision


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("state", type=Path)
    args = parser.parse_args()
    print(f"tui-{stamp(args.package, args.state)}")
