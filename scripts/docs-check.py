#!/usr/bin/env python3
"""Validate the handbook manifest, coverage, frontmatter, and nav files.

This is a minimal first pass:
- verify YAML syntax
- verify required top-level keys
- verify chapter path registration
- verify coverage topic registration
- verify chapter frontmatter and evidence
- verify generated nav matches the manifest
- verify verified chapters are not stale relative to changed code evidence
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections import Counter
from datetime import date
from pathlib import Path

import yaml

from docs_booklib import (
    ALLOWED_BOOK_STATUSES,
    ALLOWED_CHAPTER_KINDS,
    ALLOWED_CHAPTER_STATUSES,
    ALLOWED_EVIDENCE_TYPES,
    COVERAGE_PATH,
    MANIFEST_PATH,
    NAV_PATH,
    ROOT,
    build_nav,
    coerce_date,
    dump_yaml,
    load_coverage,
    load_manifest,
    parse_frontmatter,
    parse_ref,
    validate_ref_exists,
)


def validate_manifest(manifest: dict) -> list[str]:
    errors: list[str] = []
    for key in ("version", "book", "chapters"):
        if key not in manifest:
            errors.append(f"manifest missing key: {key}")
    book = manifest.get("book")
    if not isinstance(book, dict):
        errors.append("manifest.book must be a mapping")
    else:
        for key in ("id", "title", "language", "root", "nav_mode", "status"):
            if key not in book:
                errors.append(f"manifest.book missing key: {key}")
        if book.get("status") not in ALLOWED_BOOK_STATUSES:
            errors.append(f"manifest.book.status must be one of {sorted(ALLOWED_BOOK_STATUSES)}")
        root = book.get("root")
        if isinstance(root, str):
            if not (ROOT / root).exists():
                errors.append(f"manifest.book.root does not exist: {root}")

    chapters = manifest.get("chapters")
    if not isinstance(chapters, list):
        errors.append("manifest.chapters must be a list")
        chapters = []

    seen_ids: set[str] = set()
    seen_paths: set[str] = set()
    seen_orders: set[int] = set()
    for idx, chapter in enumerate(chapters):
        if not isinstance(chapter, dict):
            errors.append(f"chapter[{idx}] must be a mapping")
            continue
        for key in (
            "kind",
            "id",
            "title",
            "path",
            "order",
            "status",
            "owner",
            "primary_topic",
            "depends_on",
            "aliases",
            "review_due",
        ):
            if key not in chapter:
                errors.append(f"chapter[{idx}] missing key: {key}")
        kind = chapter.get("kind")
        if kind not in ALLOWED_CHAPTER_KINDS:
            errors.append(f"chapter[{idx}].kind must be one of {sorted(ALLOWED_CHAPTER_KINDS)}")
        status = chapter.get("status")
        if status not in ALLOWED_CHAPTER_STATUSES:
            errors.append(f"chapter[{idx}].status must be one of {sorted(ALLOWED_CHAPTER_STATUSES)}")
        chap_id = chapter.get("id")
        if isinstance(chap_id, str):
            if chap_id in seen_ids:
                errors.append(f"duplicate chapter id: {chap_id}")
            seen_ids.add(chap_id)
        order = chapter.get("order")
        if isinstance(order, int):
            if order in seen_orders:
                errors.append(f"duplicate chapter order: {order}")
            seen_orders.add(order)
        else:
            errors.append(f"chapter[{idx}].order must be an integer")
        path = chapter.get("path")
        if isinstance(path, str):
            if path in seen_paths:
                errors.append(f"duplicate chapter path: {path}")
            seen_paths.add(path)
            abs_path = ROOT / path
            if not abs_path.exists():
                errors.append(f"chapter path does not exist: {path}")
        aliases = chapter.get("aliases", [])
        if aliases is not None and not isinstance(aliases, list):
            errors.append(f"chapter[{idx}].aliases must be a list")
        topics = chapter.get("topics", [])
        if topics is not None and not isinstance(topics, list):
            errors.append(f"chapter[{idx}].topics must be a list")
        depends_on = chapter.get("depends_on", [])
        if depends_on is not None and not isinstance(depends_on, list):
            errors.append(f"chapter[{idx}].depends_on must be a list")
        review_due = chapter.get("review_due")
        if review_due is None:
            continue
        try:
            due = coerce_date(review_due, field="review_due", path=ROOT / str(path))
        except ValueError as exc:
            errors.append(str(exc))
            continue
        if isinstance(chapter.get("last_reviewed"), (str, date)):
            try:
                reviewed = coerce_date(
                    chapter["last_reviewed"], field="last_reviewed", path=ROOT / str(path)
                )
                if due < reviewed:
                    errors.append(
                        f"chapter[{idx}].review_due must not be before last_reviewed"
                    )
            except ValueError as exc:
                errors.append(str(exc))
    return errors


def validate_chapter_frontmatter(manifest: dict) -> list[str]:
    errors: list[str] = []
    chapters = manifest.get("chapters", [])
    if not isinstance(chapters, list):
        return ["manifest.chapters must be a list"]

    manifest_by_id = {
        chapter["id"]: chapter
        for chapter in chapters
        if isinstance(chapter, dict) and isinstance(chapter.get("id"), str)
    }

    for chapter_id, chapter in manifest_by_id.items():
        path = ROOT / chapter["path"]
        try:
            parsed = parse_frontmatter(path)
        except (FileNotFoundError, ValueError) as exc:
            errors.append(str(exc))
            continue

        fm = parsed.data
        required_keys = (
            "id",
            "title",
            "kind",
            "status",
            "owner",
            "primary_topic",
            "topics",
            "depends_on",
            "aliases",
            "evidence",
            "last_reviewed",
            "review_due",
        )
        for key in required_keys:
            if key not in fm:
                errors.append(f"{path}: frontmatter missing key: {key}")

        for key in ("id", "title", "kind", "status", "owner", "primary_topic"):
            if key in fm and fm.get(key) != chapter.get(key):
                errors.append(
                    f"{path}: frontmatter {key} mismatch: manifest={chapter.get(key)!r}, file={fm.get(key)!r}"
                )

        for key in ("topics", "depends_on", "aliases"):
            value = fm.get(key, [])
            if value is not None and not isinstance(value, list):
                errors.append(f"{path}: frontmatter {key} must be a list")

        evidence = fm.get("evidence", [])
        if evidence is not None and not isinstance(evidence, list):
            errors.append(f"{path}: frontmatter evidence must be a list")
            evidence = []

        if parsed.body:
            first_nonempty = next(
                (line.strip() for line in parsed.body.splitlines() if line.strip()), ""
            )
            expected_heading = f"# {chapter.get('title')}"
            if first_nonempty != expected_heading:
                errors.append(
                    f"{path}: first heading mismatch: expected {expected_heading!r}, got {first_nonempty!r}"
                )
        else:
            errors.append(f"{path}: chapter body is empty")

        if fm.get("status") == "verified" and not evidence:
            errors.append(f"{path}: verified chapter must include non-empty evidence")

        for idx, entry in enumerate(evidence):
            if not isinstance(entry, dict):
                errors.append(f"{path}: evidence[{idx}] must be a mapping")
                continue
            for key in ("type", "ref"):
                if key not in entry:
                    errors.append(f"{path}: evidence[{idx}] missing key: {key}")
            ev_type = entry.get("type")
            if ev_type not in ALLOWED_EVIDENCE_TYPES:
                errors.append(
                    f"{path}: evidence[{idx}].type must be one of {sorted(ALLOWED_EVIDENCE_TYPES)}"
                )
            ref = entry.get("ref")
            if isinstance(ref, str):
                errors.extend(validate_ref_exists(ref))
            else:
                errors.append(f"{path}: evidence[{idx}].ref must be a string")

        if fm.get("status") == "verified" and evidence:
            if not any(entry.get("type") in {"code", "test"} for entry in evidence if isinstance(entry, dict)):
                errors.append(f"{path}: verified chapter must include at least one code/test evidence item")

    return errors


def validate_coverage(coverage: dict, manifest: dict) -> list[str]:
    errors: list[str] = []
    if "version" not in coverage:
        errors.append("coverage missing key: version")
    topics = coverage.get("required_topics")
    if not isinstance(topics, list):
        errors.append("coverage.required_topics must be a list")
        topics = []

    chapters = manifest.get("chapters", [])
    if not isinstance(chapters, list):
        errors.append("manifest.chapters must be a list")
        chapters = []

    chapters_by_id = {
        chapter["id"]: chapter
        for chapter in chapters
        if isinstance(chapter, dict) and isinstance(chapter.get("id"), str)
    }
    chapter_ids = set(chapters_by_id)

    seen_topics: set[str] = set()
    for idx, topic in enumerate(topics):
        if not isinstance(topic, dict):
            errors.append(f"required_topics[{idx}] must be a mapping")
            continue
        for key in ("id", "title", "required_chapters", "evidence_min"):
            if key not in topic:
                errors.append(f"required_topics[{idx}] missing key: {key}")
        topic_id = topic.get("id")
        if isinstance(topic_id, str):
            if topic_id in seen_topics:
                errors.append(f"duplicate required topic id: {topic_id}")
            seen_topics.add(topic_id)
        required_chapters = topic.get("required_chapters", [])
        if not isinstance(required_chapters, list):
            errors.append(f"required_topics[{idx}].required_chapters must be a list")
            continue
        missing_topic_chapters = [
            chapter_id for chapter_id in required_chapters if chapter_id not in chapter_ids
        ]
        for chapter_id in missing_topic_chapters:
            errors.append(f"coverage references unknown chapter id: {chapter_id}")

        for chapter_id in required_chapters:
            if chapter_id not in chapters_by_id:
                continue

        topic_chapters = [chapters_by_id[chapter_id] for chapter_id in required_chapters if chapter_id in chapters_by_id]
        if topic_chapters and all(chapter.get("status") == "verified" for chapter in topic_chapters):
            evidence_counts: Counter[str] = Counter()
            for chapter in topic_chapters:
                try:
                    parsed = parse_frontmatter(ROOT / chapter["path"])
                except (FileNotFoundError, ValueError) as exc:
                    errors.append(str(exc))
                    continue
                evidence = parsed.data.get("evidence", [])
                if not isinstance(evidence, list):
                    errors.append(f"{chapter['path']}: frontmatter evidence must be a list")
                    continue
                for entry in evidence:
                    if isinstance(entry, dict) and isinstance(entry.get("type"), str):
                        evidence_counts[entry["type"]] += 1

            evidence_min = topic.get("evidence_min", {})
            if isinstance(evidence_min, dict):
                for evidence_type, minimum in evidence_min.items():
                    if not isinstance(minimum, int):
                        errors.append(
                            f"required_topics[{idx}].evidence_min[{evidence_type}] must be an integer"
                        )
                        continue
                    if evidence_counts[evidence_type] < minimum:
                        errors.append(
                            f"required_topics[{idx}] needs at least {minimum} {evidence_type} evidence item(s), got {evidence_counts[evidence_type]}"
                        )
            else:
                errors.append(f"required_topics[{idx}].evidence_min must be a mapping")
    return errors


def validate_nav(manifest: dict) -> list[str]:
    errors: list[str] = []
    expected = build_nav(manifest)
    if not NAV_PATH.exists():
        errors.append(f"missing generated nav file: {NAV_PATH}")
        return errors

    try:
        with NAV_PATH.open("r", encoding="utf-8") as fh:
            actual = yaml.safe_load(fh)
    except yaml.YAMLError as exc:
        errors.append(f"invalid yaml in {NAV_PATH}: {exc}")
        return errors

    if actual != expected:
        errors.append(
            "generated nav file is stale; run scripts/docs-build-nav.py to refresh"
        )
    return errors


def git_changed_paths() -> set[Path]:
    try:
        proc = subprocess.run(
            [
                "git",
                "-C",
                str(ROOT),
                "ls-files",
                "-m",
                "-o",
                "--exclude-standard",
            ],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return set()

    changed: set[Path] = set()
    for line in proc.stdout.splitlines():
        path = line.strip()
        if path:
            changed.add((ROOT / path).resolve())
    return changed


def validate_stale_chapters(manifest: dict, changed_paths: set[Path]) -> list[str]:
    errors: list[str] = []
    if not changed_paths:
        return errors

    chapters = manifest.get("chapters", [])
    if not isinstance(chapters, list):
        return ["manifest.chapters must be a list"]

    for chapter in chapters:
        if not isinstance(chapter, dict):
            continue
        if chapter.get("status") != "verified":
            continue

        chapter_path = (ROOT / str(chapter.get("path", ""))).resolve()
        if chapter_path in changed_paths:
            continue

        try:
            parsed = parse_frontmatter(chapter_path)
        except (FileNotFoundError, ValueError):
            continue

        stale_refs: list[str] = []
        for entry in parsed.data.get("evidence", []):
            if not isinstance(entry, dict):
                continue
            if entry.get("type") not in {"code", "test"}:
                continue
            ref = entry.get("ref")
            if not isinstance(ref, str):
                continue
            if validate_ref_exists(ref):
                continue
            try:
                from_ref = parse_ref(ref).path.resolve()
            except ValueError:
                continue
            if from_ref in changed_paths:
                stale_refs.append(ref)

        if stale_refs:
            errors.append(
                f"{chapter_path}: verified chapter is stale because referenced code/test evidence changed: {', '.join(sorted(stale_refs))}"
            )

    return errors


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--json", action="store_true", help="emit json report")
    parser.add_argument(
        "--fail-on-stale",
        action="store_true",
        help="treat stale chapter detections as errors",
    )
    args = parser.parse_args(argv)

    try:
        manifest = load_manifest()
        coverage = load_coverage()
    except (FileNotFoundError, ValueError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1

    errors = []
    errors.extend(validate_manifest(manifest))
    errors.extend(validate_chapter_frontmatter(manifest))
    errors.extend(validate_coverage(coverage, manifest))
    errors.extend(validate_nav(manifest))
    stale_errors = validate_stale_chapters(manifest, git_changed_paths())
    if args.fail_on_stale:
        errors.extend(stale_errors)

    result = {
        "manifest": str(MANIFEST_PATH),
        "coverage": str(COVERAGE_PATH),
        "nav": str(NAV_PATH),
        "ok": not errors,
        "errors": errors,
        "warnings": stale_errors if not args.fail_on_stale else [],
    }

    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        if errors:
            for error in errors:
                print(f"ERROR: {error}", file=sys.stderr)
        if stale_errors and not args.fail_on_stale:
            for warning in stale_errors:
                print(f"WARN: {warning}", file=sys.stderr)
        if not errors:
            print("docs-check: ok")

    return 0 if not errors else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
