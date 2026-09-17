#!/usr/bin/env python3
"""loc.py — authoritative line counter for the repo.

Counts lines by iterating the file handle (no split(), no shell encoding
guessing), so the result matches `wc -l` and is stable across platforms.

Usage:
    python3 scripts/loc.py [root] [extension]

    root       directory to walk            (default: .)
    extension  file suffix to count         (default: .rs)

Examples:
    python3 scripts/loc.py src .rs     # Rust  -> TOTAL=6621
    python3 scripts/loc.py . .py       # Python

Why this exists: three different figures (6,636 / 6,621 / 5,936) were once
quoted for the same tree because of (a) split("\\n") adding a phantom line
per file, and (b) PowerShell's Get-Content decoding UTF-8 source as GBK.
This script is the single source of truth.
"""
import os
import sys

root = sys.argv[1] if len(sys.argv) > 1 else "."
ext = sys.argv[2] if len(sys.argv) > 2 else ".rs"
if not ext.startswith("."):
    ext = "." + ext

total = 0
nfiles = 0
rows = []
for dp, _, fs in os.walk(root):
    for f in fs:
        if f.endswith(ext):
            p = os.path.join(dp, f)
            with open(p, encoding="utf-8") as fh:
                n = sum(1 for _ in fh)
            total += n
            nfiles += 1
            rows.append((n, os.path.relpath(p, root)))

for n, rel in sorted(rows, key=lambda r: -r[0]):
    print(f"{n:>6}  {rel}")

print(f"\nfiles={nfiles}  ext={ext}  TOTAL={total}")
