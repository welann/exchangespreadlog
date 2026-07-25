#!/usr/bin/env python3
"""Render a candle migration template without connecting to ClickHouse."""

from __future__ import annotations

import argparse
import re
from pathlib import Path


IDENTIFIER = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def identifier(value: str) -> str:
    if not IDENTIFIER.fullmatch(value):
        raise argparse.ArgumentTypeError("must be a simple ClickHouse identifier")
    return value


def positive_int(value: str) -> int:
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return parsed


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("template", type=Path)
    parser.add_argument("--database", required=True, type=identifier)
    parser.add_argument("--table", required=True, type=identifier)
    parser.add_argument("--cutover-recv-ts-ns", required=True, type=positive_int)
    parser.add_argument("--from-ms", type=positive_int)
    parser.add_argument("--to-ms", type=positive_int)
    parser.add_argument("--coverage-from-ms", type=positive_int)
    parser.add_argument("--coverage-to-ms", type=positive_int)
    args = parser.parse_args()

    values = {
        "database": args.database,
        "table": args.table,
        "cutover_recv_ts_ns": str(args.cutover_recv_ts_ns),
        "from_ms": optional(args.from_ms),
        "to_ms": optional(args.to_ms),
        "coverage_from_ms": optional(args.coverage_from_ms),
        "coverage_to_ms": optional(args.coverage_to_ms),
    }
    rendered = args.template.read_text(encoding="utf-8")
    for key, value in values.items():
        if value is not None:
            rendered = rendered.replace("{" + key + "}", value)

    unresolved = sorted(set(re.findall(r"\{([a-z0-9_]+)\}", rendered)))
    if unresolved:
        parser.error("missing values for: " + ", ".join(unresolved))
    print(rendered, end="")


def optional(value: int | None) -> str | None:
    return None if value is None else str(value)


if __name__ == "__main__":
    main()
