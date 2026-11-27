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
"""Fetch/compare Lyrnc's pinned synthetic oracle without vendoring its LOCAL files.

Source and native gfx1036 measurements belong to Lyrnc; see the gist README.
Only the firstlane_branch cases are selected. No title inputs are accepted.
"""
import hashlib
import json
from pathlib import Path
import re
import sys
import urllib.request

GIST = "https://gist.githubusercontent.com/Lyrnc/00c6589f26930d5d07c077f8c4a30131/raw/"
FILES = {
    "fw64_data.rs": ("f4383c6d310fd40ed8c36c93d547f0f1f36a16be", "34dba351876f2103a33e91dc16145788e976ba7bb7b0ddf87839e6ceb4a7c59c"),
    "oracle_fw64.json": ("ce2f401463491742b10bb36d339c5d1e5103efbf", "eee0f705651c6083e3fbd8a0e3645ff55b66ed9222a68556b12a497477588715"),
}

def prepare(out, paths=None):
    repo = Path(os.environ["PAIRED_REPRO_ROOT"])
    out = out.resolve()
    if out == repo or repo in out.parents:
        raise SystemExit("Keep downloaded oracle inputs and outputs outside the checkout")
    out.mkdir(parents=True, exist_ok=True)
    content = {}
    for i, (name, (rev, expected)) in enumerate(FILES.items()):
        if paths:
            data = Path(paths[i]).read_bytes()
        else:
            with urllib.request.urlopen(GIST + rev + "/" + name, timeout=30) as response:
                data = response.read()
        actual = hashlib.sha256(data).hexdigest()
        if actual != expected:
            raise SystemExit(f"{name}: checksum mismatch: {actual}")
        content[name] = data.decode("utf-8")
        (out / name).write_bytes(data)
    cases = re.findall(r'\("(firstlane_branch/[^\"]+)",\s*(\d+),\s*&\[(.*?)\]\)', content["fw64_data.rs"])
    assert len(cases) == 10
    rows = []
    reference = json.loads(content["oracle_fw64.json"])
    for name, lds, words in cases:
        words = [int(word.strip(), 16) for word in words.split(",") if word.strip()]
        assert len(reference[name]) == 64 and all(len(row) == 16 for row in reference[name])
        rows.append(name + "\t" + lds + "\t" + ",".join(f"{word:x}" for word in words))
    (out / "firstlane-cases.tsv").write_text("\n".join(rows) + "\n", encoding="utf-8")
    print(f"Prepared {len(cases)} attributed synthetic cases at {out}")

def compare(actual_path, reference_path):
    reference = json.loads(Path(reference_path).read_text(encoding="utf-8"))
    seen = set()
    compared = wrong = refused = 0
    for line in Path(actual_path).read_text(encoding="utf-8").splitlines():
        name, status, data = line.split("\t", 2)
        assert name.startswith("firstlane_branch/") and name not in seen
        seen.add(name)
        if status != "OK":
            refused += 1
            print(f"{name}: {status}: {data}")
            continue
        actual = [int(word, 16) for word in data.split(",")]
        want = [reference[name][lane][reg] for reg in range(16) for lane in range(64)]
        assert len(actual) == len(want) == 1024, "complete register-major output required"
        bad = [(i, got, expected) for i, (got, expected) in enumerate(zip(actual, want)) if got != expected]
        print(f"{name}: {len(bad)}/1024 wrong; " + str([
            {"register": 10 + i // 64, "lane": i % 64, "got": got, "expected": expected}
            for i, got, expected in bad[:3]
        ]))
        compared += 1024
        wrong += len(bad)
    assert len(seen) == 10, "all ten masks must be reported"
    print(f"FIRSTLANE_SUMMARY compared={compared} wrong={wrong} refused={refused}")
    return int(bool(wrong or refused))

if __name__ == "__main__":
    command, *args = sys.argv[1:]
    if command == "fetch" and len(args) == 1:
        prepare(Path(args[0]))
    elif command == "prepare" and len(args) == 3:
        prepare(Path(args[0]), args[1:])
    elif command == "compare" and len(args) == 2:
        raise SystemExit(compare(*args))
    else:
        raise SystemExit("fetch OUTDIR | prepare OUTDIR DATA REFERENCE | compare ACTUAL REFERENCE")

PAIRED_REPRO_PYTHON
