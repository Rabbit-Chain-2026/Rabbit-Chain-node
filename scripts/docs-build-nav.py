#!/usr/bin/env python3
"""Generate docs/nav.generated.yml from docs/manifest.yaml."""

from __future__ import annotations

import argparse
from pathlib import Path
import sys

from docs_booklib import MANIFEST_PATH, NAV_PATH, build_nav, dump_yaml, load_manifest


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--output",
        type=Path,
        default=NAV_PATH,
        help="nav output path (default: docs/nav.generated.yml)",
    )
    parser.add_argument(
        "--stdout",
        action="store_true",
        help="print generated nav to stdout instead of writing a file",
    )
    args = parser.parse_args(argv)

    manifest = load_manifest()
    nav = build_nav(manifest)
    payload = dump_yaml(nav)

    if args.stdout:
        sys.stdout.write(payload)
        return 0

    args.output.write_text(payload, encoding="utf-8")
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
