#!/usr/bin/env python3
"""Shared helpers for the handbook manifest, coverage, and nav files."""

from __future__ import annotations

from collections import OrderedDict
from dataclasses import dataclass
from datetime import date, datetime
from pathlib import Path
import re
from typing import Any

import yaml


ROOT = Path(__file__).resolve().parents[1]
DOCS_DIR = ROOT / "docs"
MANIFEST_PATH = DOCS_DIR / "manifest.yaml"
COVERAGE_PATH = DOCS_DIR / "coverage.yaml"
NAV_PATH = DOCS_DIR / "nav.generated.yml"

SECTION_LABELS = {
    "00-preface": "序章",
    "10-foundations": "基础",
    "20-transaction-path": "交易路径",
    "30-block-model": "区块模型",
    "40-storage": "存储",
    "40-network": "网络",
    "50-sync": "同步",
    "60-mining": "挖矿",
    "70-operations": "运行与运维",
    "80-security": "安全",
    "90-appendix": "附录",
}

ALLOWED_CHAPTER_STATUSES = {"draft", "verified", "stale", "archived"}
ALLOWED_BOOK_STATUSES = {"active", "draft", "archived"}
ALLOWED_CHAPTER_KINDS = {"chapter", "appendix", "legacy"}
ALLOWED_EVIDENCE_TYPES = {"code", "test", "log", "report", "doc", "benchmark"}


@dataclass(frozen=True)
class ParsedFrontmatter:
    data: dict[str, Any]
    body: str


@dataclass(frozen=True)
class ParsedRef:
    path: Path
    start_line: int | None = None
    end_line: int | None = None


def load_yaml(path: Path) -> dict[str, Any]:
    try:
        with path.open("r", encoding="utf-8") as fh:
            data = yaml.safe_load(fh)
    except FileNotFoundError:
        raise FileNotFoundError(f"missing file: {path}") from None
    except yaml.YAMLError as exc:
        raise ValueError(f"invalid yaml in {path}: {exc}") from exc
    if not isinstance(data, dict):
        raise ValueError(f"yaml root must be a mapping: {path}")
    return data


def load_manifest() -> dict[str, Any]:
    return load_yaml(MANIFEST_PATH)


def load_coverage() -> dict[str, Any]:
    return load_yaml(COVERAGE_PATH)


def parse_frontmatter(path: Path) -> ParsedFrontmatter:
    try:
        text = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        raise FileNotFoundError(f"missing file: {path}") from None

    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        raise ValueError(f"missing frontmatter start marker: {path}")

    end_idx = None
    for idx in range(1, len(lines)):
        if lines[idx].strip() == "---":
            end_idx = idx
            break

    if end_idx is None:
        raise ValueError(f"missing frontmatter end marker: {path}")

    fm_text = "\n".join(lines[1:end_idx])
    body = "\n".join(lines[end_idx + 1 :])

    try:
        data = yaml.safe_load(fm_text)
    except yaml.YAMLError as exc:
        raise ValueError(f"invalid frontmatter yaml in {path}: {exc}") from exc
    if not isinstance(data, dict):
        raise ValueError(f"frontmatter must be a mapping: {path}")

    return ParsedFrontmatter(data=data, body=body)


def coerce_date(value: Any, *, field: str, path: Path) -> date:
    if isinstance(value, datetime):
        return value.date()
    if isinstance(value, date):
        return value
    if isinstance(value, str):
        try:
            return date.fromisoformat(value)
        except ValueError as exc:
            raise ValueError(f"invalid {field} date in {path}: {value}") from exc
    raise ValueError(f"{field} must be a date or YYYY-MM-DD string in {path}")


def parse_ref(ref: str) -> ParsedRef:
    if ref.startswith(("http://", "https://")):
        raise ValueError(f"external refs are not allowed: {ref}")

    match = re.match(
        r"^(?P<path>.*?)(?:(?:#L(?P<start1>\d+)(?:-L(?P<end1>\d+))?)|"
        r"(?:[:](?P<start2>\d+)(?:-(?P<end2>\d+))?))?$",
        ref,
    )
    if not match:
        raise ValueError(f"invalid evidence ref: {ref}")

    raw_path = match.group("path")
    if not raw_path:
        raise ValueError(f"empty evidence path: {ref}")

    start = match.group("start1") or match.group("start2")
    end = match.group("end1") or match.group("end2")
    start_line = int(start) if start else None
    end_line = int(end) if end else start_line

    path = Path(raw_path)
    if not path.is_absolute():
        path = ROOT / path

    return ParsedRef(path=path, start_line=start_line, end_line=end_line)


def validate_ref_exists(ref: str) -> list[str]:
    errors: list[str] = []
    try:
        parsed = parse_ref(ref)
    except ValueError as exc:
        return [str(exc)]

    if not parsed.path.exists():
        return [f"evidence ref target does not exist: {ref}"]

    if parsed.start_line is None:
        return errors

    try:
        line_count = len(parsed.path.read_text(encoding="utf-8").splitlines())
    except UnicodeDecodeError:
        return [f"evidence ref target is not UTF-8 text: {ref}"]

    if parsed.start_line < 1 or parsed.end_line is None:
        return [f"invalid evidence line span: {ref}"]
    if parsed.end_line < parsed.start_line:
        return [f"invalid evidence line span: {ref}"]
    if parsed.end_line > line_count:
        return [f"evidence line span out of range in {ref}: file has {line_count} lines"]

    return errors


def chapter_group_key(path: str) -> str:
    parts = Path(path).parts
    if len(parts) < 3:
        raise ValueError(f"chapter path too short: {path}")
    return parts[2]


def build_nav(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    chapters = manifest.get("chapters", [])
    if not isinstance(chapters, list):
        raise ValueError("manifest.chapters must be a list")

    grouped: "OrderedDict[str, list[dict[str, Any]]]" = OrderedDict()
    for chapter in sorted(chapters, key=lambda item: (item.get("order", 0), item.get("id", ""))):
        if not isinstance(chapter, dict):
            continue
        path = chapter.get("path")
        if not isinstance(path, str):
            continue
        group_key = chapter_group_key(path)
        grouped.setdefault(group_key, []).append(chapter)

    nav: list[dict[str, Any]] = []
    for group_key, items in grouped.items():
        items = sorted(items, key=lambda item: (item.get("order", 0), item.get("title", "")))
        if len(items) == 1:
            chapter = items[0]
            nav.append({chapter["title"]: chapter["path"]})
            continue

        label = SECTION_LABELS.get(group_key, group_key.replace("-", " ").title())
        nav.append({label: [{chapter["title"]: chapter["path"]} for chapter in items]})

    return nav


def dump_yaml(data: Any) -> str:
    return yaml.safe_dump(data, sort_keys=False, allow_unicode=True)
