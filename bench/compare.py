"""Compare a benchmark run with the committed baseline.

    python bench/compare.py bench/baseline.json result.json

Output size is deterministic: growing more than SIZE_TOLERANCE fails. Encode time depends on the
machine; it is printed, and with --warn-time growing more than TIME_TOLERANCE prints a GitHub
Actions warning. Only use --warn-time once the baseline times come from the same kind of machine
(e.g. a CI run's bench-result.json), otherwise every run warns.
Refresh the baseline with: cargo run --release -p zzalcam-encode --example bench -- --json bench/baseline.json
"""
import json
import sys

SIZE_TOLERANCE = 0.02
TIME_TOLERANCE = 0.30


def main(baseline_path: str, result_path: str, warn_time: bool) -> int:
    baseline = json.load(open(baseline_path, encoding="utf-8"))
    result = json.load(open(result_path, encoding="utf-8"))
    failed = False
    print(f"{'case':22} {'size':>12} {'diff':>8} {'time':>8} {'diff':>8}")
    for name, base in baseline.items():
        cur = result.get(name)
        if cur is None:
            print(f"::error::benchmark case {name} is missing")
            failed = True
            continue
        dsize = cur["bytes"] / base["bytes"] - 1
        dtime = cur["secs"] / base["secs"] - 1
        print(f"{name:22} {cur['bytes']:12,} {dsize:+8.1%} {cur['secs']:7.2f}s {dtime:+8.1%}")
        if dsize > SIZE_TOLERANCE:
            print(f"::error::{name}: output grew {dsize:+.1%} (limit {SIZE_TOLERANCE:.0%})")
            failed = True
        if warn_time and dtime > TIME_TOLERANCE:
            print(f"::warning::{name}: encode time grew {dtime:+.1%} (runner noise is possible)")
    return 1 if failed else 0


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if a != "--warn-time"]
    sys.exit(main(args[0], args[1], "--warn-time" in sys.argv))
