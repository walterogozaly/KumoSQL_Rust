# Baseline for the Python reference repository

The reference repository `C:/Users/walte/Desktop/Repos/KumoSQL` is **not** clean
at `fbfff31`, the first commit of this port. It arrived dirty.

This file records that pre-existing state so that "I did not touch the reference
repo" can be checked exactly, instead of by eye.

## What was already modified

One file, one line, uncommitted, last written **2026-10-05 00:55:17**, roughly
thirty minutes before the first commit of this repository (01:25 onwards):

```
 M docs/evals/sqlsolver.md
```

The edit is a typo in the "Algebraic normal form" bullet of the SQLSolver plan:

```diff
-... projections distribute over `UNION ALL`; ...
+... projections didthedistribute over `UNION ALL`; ...
```

`distribute` became `didthedistribute`. Nothing else in the repository differs
from `362545a3`.

This port did **not** cause this, and deliberately did **not** repair it: the
reference repository is read-only under the project contract, so reverting it
would itself be a write to a repo that must stay untouched. Reverting is the
owner's call, not this project's.

## The check

```shell
python tools/check_reference_clean.py
```

It compares the reference repo's diff against the recorded baseline and exits
non-zero if anything *else* changed. Exit codes:

| Code | Meaning |
| --- | --- |
| 0 | Reference repo matches the baseline: nothing beyond the recorded typo |
| 1 | The reference repo has extra modifications beyond the baseline |
| 2 | The reference repo is cleaner than the baseline (the typo was fixed) -- fine, not a failure |

Once the owner reverts the typo, this baseline should be updated to empty and
the check should require a fully clean tree.