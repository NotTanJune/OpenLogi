#!/usr/bin/env python3
"""Verify a private working-display snapshot without changing files or settings."""

import argparse
import hashlib
import json
from pathlib import Path


def inventory(base):
    """Record every directory, symlink target, file size, and file digest."""
    entries = {}
    paths = [base] if base.is_file() or base.is_symlink() else [base, *sorted(base.rglob("*"))]
    for path in paths:
        key = "." if path == base else str(path.relative_to(base))
        if path.is_symlink():
            entries[key] = {"link": str(path.readlink())}
        elif path.is_file():
            entries[key] = {
                "bytes": path.stat().st_size,
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
        elif path.is_dir():
            entries[key] = {"directory": True}
    return entries


def verify(label, path, expected):
    actual = inventory(path)
    if actual != expected:
        print(f"FAIL {label}: {path}")
        for name in sorted(set(actual) | set(expected)):
            if actual.get(name) != expected.get(name):
                print(f"  changed or missing: {name}")
        return False
    files = sum("bytes" in entry for entry in actual.values())
    size = sum(entry.get("bytes", 0) for entry in actual.values())
    print(f"PASS {label}: {files} files, {size} bytes, all SHA-256 hashes match")
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("snapshot", type=Path, help="Directory containing manifest.json")
    parser.add_argument("--installed", action="store_true", help="Also compare original source paths")
    args = parser.parse_args()
    manifest = json.loads((args.snapshot / "manifest.json").read_text())
    if not manifest:
        raise ValueError("Snapshot manifest is empty")
    success = True
    for name, item in manifest.items():
        success = verify(f"snapshot/{name}", args.snapshot / name, item["entries"]) and success
        if args.installed and "source" in item:
            success = verify(f"installed/{name}", Path(item["source"]), item["entries"]) and success
    return 0 if success else 1


if __name__ == "__main__":
    raise SystemExit(main())
