#!/usr/bin/env python3
"""Check that the Python reference repository has not been modified by this port.

The reference repo is read-only under the project contract, but it was *not*
clean when this port started: `docs/evals/sqlsolver.md` carried one
pre-existing typo edit (see docs/reference-baseline.md). So a plain
`git status --porcelain` emptiness test cannot be the check -- it would fail for
a condition this port did not cause.

Instead this compares the reference repo's diff against the recorded baseline
sha256. Anything beyond the baseline is a real violation.

Exit codes:
    0  reference repo matches the baseline (nothing beyond the recorded typo)
    1  reference repo has modifications beyond the baseline  -> VIOLATION
    2  reference repo is cleaner than the baseline (baseline is stale)
    3  the reference repo or git is unavailable
"""

from __future__ import annotations

import hashlib
import subprocess
import sys
from pathlib import Path

REFERENCE_REPO = Path("../KumoSQL").resolve()

# sha256 of `git diff` output as captured at this port's first commit (fbfff31).
BASELINE_DIFF_SHA256 = (
    "d05fd6b0c96e182ec524dac20db4abbe72836f65eba41b0091a6a2027c6c6c2d"
)


def run_git(*args: str) -> str:
    """Run git and return stdout as text with normalised line endings.

    `text=True` is deliberately avoided: on Windows it makes the resulting
    newline handling depend on how git happened to emit the bytes, which
    silently changed the digest between runs. Decode explicitly and normalise
    CRLF to LF instead, so the hash is stable.
    """
    result = subprocess.run(
        ["git", "-C", str(REFERENCE_REPO), *args],
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        err = result.stderr.decode("utf-8", "replace").strip()
        print(f"git {' '.join(args)} failed: {err}", file=sys.stderr)
        raise SystemExit(3)
    return result.stdout.decode("utf-8", "replace").replace("\r\n", "\n")


def main() -> int:
    if not (REFERENCE_REPO / ".git").exists():
        print(f"reference repo not found at {REFERENCE_REPO}", file=sys.stderr)
        return 3

    diff = run_git("diff")
    if not diff.strip():
        print("reference repo is clean (cleaner than the recorded baseline)")
        return 2

    digest = hashlib.sha256(diff.encode("utf-8")).hexdigest()
    status = run_git("status", "--porcelain").strip()

    if digest == BASELINE_DIFF_SHA256:
        print("OK: reference repo matches the recorded baseline exactly.")
        print("     pre-existing (not caused by this port):")
        for line in status.splitlines():
            print(f"       {line}")
        return 0

    print("VIOLATION: the reference repo differs beyond the recorded baseline.", file=sys.stderr)
    print(f"  expected sha256 {BASELINE_DIFF_SHA256}", file=sys.stderr)
    print(f"  actual   sha256 {digest}", file=sys.stderr)
    print("  status:", file=sys.stderr)
    for line in status.splitlines():
        print(f"    {line}", file=sys.stderr)
    print(
        "\n  If you changed the reference repo deliberately, update\n"
        "  docs/reference-baseline.md and this constant in the same change.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())