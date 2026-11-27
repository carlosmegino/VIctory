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
"""Bounded synthetic test runs; preserve failures instead of weakening oracles."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

parser = argparse.ArgumentParser()
parser.add_argument("executable", type=Path)
parser.add_argument("output", type=Path, help="directory outside this checkout")
parser.add_argument("--wine", action="store_true", help="use Wine; explicitly omit Windows debug layer/GBV")
parser.add_argument("--suite", choices=("contract", "consumers", "all"), default="all")
args = parser.parse_args()
repo = Path(os.environ["PAIRED_REPRO_ROOT"])
out, exe = args.output.resolve(), args.executable.resolve()
if out == repo or repo in out.parents:
    raise SystemExit("Keep logs and generated inputs outside the checkout")
out.mkdir(parents=True, exist_ok=True)
env = {k: v for k, v in os.environ.items() if not k.startswith("VICTORY_")}
env["VICTORY_WARP"] = "0"
env["VICTORY_D3D12_WARP"] = "0"
env["VICTORY_DXIL"] = os.environ["VICTORY_DXIL"]
if args.wine:
    env["VICTORY_REPRO_NO_DEBUG"] = "1"
prefix = ["wine"] if args.wine else []

def winpath(path):
    if args.wine:
        return subprocess.check_output(["winepath", "-w", str(path)], text=True, env=env).strip()
    return str(path)

tests = []
if args.suite in ("contract", "all"):
    tests += [
        ("contract-audit", "emit::paired_repro_contract_tests::guest_lds_dependencies_and_branches", False),
        ("contract-64", "emit::paired_repro_contract_tests::wave64_2688_group_64", True),
        ("contract-128", "emit::paired_repro_contract_tests::wave64_2688_group_128", True),
    ]
if args.suite in ("consumers", "all"):
    tests += [
        ("lane-loops", "emit::paired_repro_consumer_tests::lane_loop_consumers", True),
        ("firstlane-dispatch", "emit::paired_repro_consumer_tests::firstlane_native_oracle", True),
    ]
    assert (out / "firstlane-cases.tsv").is_file(), "run paired-repro-oracle.sh fetch OUTDIR first"
    env["VICTORY_REPRO_CASES"] = winpath(out / "firstlane-cases.tsv")
    env["VICTORY_REPRO_OUT"] = winpath(out / "firstlane-actual.tsv")

results = []
for label, test, ignored in tests:
    log = out / f"{label}.log"
    if log.exists():
        raise SystemExit(f"Refusing to overwrite existing evidence: {log}")
    command = prefix + [str(exe), "--exact", test, "--nocapture", "--test-threads=1"]
    if ignored:
        command += ["--ignored"]
    try:
        run = subprocess.run(command, env=env, cwd=repo, capture_output=True, timeout=120)
        data = run.stdout + run.stderr
        log.write_bytes(data)
        valid_run = b"running 1 test" in data and b"SKIP " not in data
        status = run.returncode if valid_run else 2
    except subprocess.TimeoutExpired as error:
        log.write_bytes((error.stdout or b"") + (error.stderr or b""))
        status = 124
    results.append({"test": test, "status": status, "log": log.name})
    print(f"{label}: exit={status}; {log}", flush=True)

if args.suite in ("consumers", "all") and (out / "firstlane-actual.tsv").exists():
    comparison = subprocess.run([
        os.environ["PAIRED_REPRO_BASH"], str(repo / "tools/paired-repro-oracle.sh"), "compare",
        str(out / "firstlane-actual.tsv"), str(out / "oracle_fw64.json"),
    ], capture_output=True)
    (out / "firstlane-comparison.log").write_bytes(comparison.stdout + comparison.stderr)
    results.append({"test": "native-gfx1036 comparison", "status": comparison.returncode, "log": "firstlane-comparison.log"})
    print(comparison.stdout.decode(errors="replace"), end="")

manifest = {
    "exact_backend": "a9ef792f6d516ee6200f4fedeb061a25153d48fd",
    "source_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip(),
    "source_status": subprocess.check_output(["git", "status", "--short"], cwd=repo, text=True),
    "executable_sha256": hashlib.sha256(exe.read_bytes()).hexdigest(),
    "wine": args.wine,
    "windows_debug_layer_requested": not args.wine,
    "gbv_requested_for_contract": not args.wine and args.suite in ("contract", "all"),
    "gbv_requested_for_consumers": False,
    "results": results,
}
(out / "run.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
raise SystemExit(int(any(r["status"] != 0 for r in results)))

PAIRED_REPRO_PYTHON
