#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Check bounded offline adapter captures against explicit corpus contracts."""

import argparse
import json
from pathlib import Path
from typing import TypeAlias, cast

Json: TypeAlias = None | bool | int | float | str | list["Json"] | dict[str, "Json"]
MAX_BYTES = 256 * 1024
MAX_CASES = 16


def rows(path: Path) -> dict[str, dict[str, Json]]:
    result: dict[str, dict[str, Json]] = {}
    with path.open("rb") as source:
        for _ in range(MAX_CASES + 1):
            line = source.readline(MAX_BYTES + 1)
            if not line:
                return result
            if len(line) > MAX_BYTES or len(result) == MAX_CASES:
                raise ValueError(f"{path}: capture bounds exceeded")
            row = cast(Json, json.loads(line))
            if not isinstance(row, dict) or not isinstance(row.get("id"), str):
                raise ValueError(f"{path}: row requires a string id")
            identifier = row["id"]
            if identifier in result:
                raise ValueError(f"{path}: duplicate id {identifier}")
            result[identifier] = row
    raise ValueError(f"{path}: capture bounds exceeded")


def pointer(value: Json, path: str) -> Json:
    if not path.startswith("/"):
        raise ValueError(f"invalid JSON pointer: {path}")
    for part in path[1:].split("/"):
        key = part.replace("~1", "/").replace("~0", "~")
        if isinstance(value, dict):
            value = value[key]
        elif isinstance(value, list):
            value = value[int(key)]
        else:
            raise ValueError(f"{path}: traverses a scalar")
    return value


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture", type=Path)
    parser.add_argument(
        "--corpus", type=Path, default=Path(__file__).with_name("corpus.jsonl")
    )
    args = parser.parse_args()
    corpus = rows(args.corpus)
    captures = rows(args.capture)
    if corpus.keys() != captures.keys():
        raise ValueError(
            f"case identities differ: expected {sorted(corpus)}, got {sorted(captures)}"
        )
    failures: list[str] = []
    for identifier, case in corpus.items():
        expected = case["expect"]
        if not isinstance(expected, dict):
            raise ValueError(f"{identifier}: invalid fixture contract")
        capture = captures[identifier]
        # A scalar Messages text body and its single text-block form are equal;
        # no text, marker, block order, signature or argument bytes are rewritten.
        request = capture.get("request")
        if isinstance(request, dict) and isinstance(request.get("messages"), list):
            for message in request["messages"]:
                if isinstance(message, dict) and isinstance(
                    message.get("content"), str
                ):
                    message["content"] = [{"type": "text", "text": message["content"]}]
        for path, wanted in expected.items():
            try:
                got = pointer(capture, path)
                matches = got == wanted
            except (KeyError, IndexError, ValueError):
                matches = False
            if not matches:
                failures.append(f"{identifier} {path}: contract mismatch")
    if failures:
        raise ValueError("\n".join(failures))
    print(f"{len(corpus)} cases passed")


if __name__ == "__main__":
    main()
