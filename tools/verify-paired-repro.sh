#!/usr/bin/env bash
set -euo pipefail
repro_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
if command -v cygpath >/dev/null 2>&1; then
    export PAIRED_REPRO_ROOT="$(cygpath -w "$repro_root")"
    export PAIRED_REPRO_BASH="$(cygpath -w "$BASH")"
else
    export PAIRED_REPRO_ROOT="$repro_root"
    export PAIRED_REPRO_BASH="$BASH"
fi
exec "${PYTHON:-python3}" - "$@" <<'PAIRED_REPRO_PYTHON'
import os
"""Verify that this companion changes no original a9ef792 source or build file."""
import hashlib
from pathlib import Path
import subprocess

BASE = "a9ef792f6d516ee6200f4fedeb061a25153d48fd"
repo = Path(os.environ["PAIRED_REPRO_ROOT"])
MOD = "crates/victory-shader/src/emit/mod.rs"
SUFFIX = b"\n#[cfg(all(test, windows))]\nmod paired_repro_contract_tests;\n#[cfg(all(test, windows))]\nmod paired_repro_consumer_tests;\n"

def git(*args):
    return subprocess.check_output(["git", *args], cwd=repo)

def function(text, name):
    start = text.index(f"fn {name}(")
    return text[start:text.index("\n}", start) + 2]

paths = git("ls-tree", "-r", "--name-only", BASE).decode().splitlines()
for path in paths:
    original = git("show", f"{BASE}:{path}").replace(b"\r\n", b"\n")
    current = (repo / path).read_bytes().replace(b"\r\n", b"\n")
    expected = original + SUFFIX if path == MOD else original
    assert current == expected, f"Original file changed: {path}"
print(f"Exact base {BASE}: {len(paths)} original files unchanged, apart from two cfg(test) declarations")

source = git("show", "1625d1c686adc74e986d578b47aa6364b672b8d6:crates/victory-shader/src/emit/wave64_group_tests.rs").decode()
adapted = (repo / "crates/victory-shader/src/emit/paired_repro_contract_tests.rs").read_text(encoding="utf-8")
for name in ("guest", "oracle"):
    a, b = function(source, name), function(adapted, name)
    assert a == b, f"#3180 {name} changed"
    print(f"#3180 {name} SHA256: {hashlib.sha256(a.encode()).hexdigest()}")

PAIRED_REPRO_PYTHON
