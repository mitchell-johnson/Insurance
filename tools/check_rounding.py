#!/usr/bin/env python3
"""Only the money crate rounds a decimal.

The decimal library's own `round` decides from the first digit it drops
and ignores the rest, so 23.9851 at two places is a tie to it. Until 14 Nov
2026 that billed about one premium in twenty a cent low. `ic_money` rounds
exactly (`round_integer`, `round_to_money`, `round_minor_to_fine`) and is
the one place allowed to touch the library's rounding. This check fails
the build if any other source file does, or if any other crate depends on
the library directly.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
ALLOWED = ROOT / "crates" / "money" / "src" / "lib.rs"
FORBIDDEN = re.compile(r"with_rounding_mode|FastRounding|fastnum::|decimal::RoundingMode")
# A decimal rounded by method call: `.round(0)`, `.round(places)`. A float's
# `.round()` takes no argument and is not matched.
DECIMAL_ROUND = re.compile(r"\.round\(\s*[A-Za-z0-9_]")


def main() -> int:
    problems = []
    for path in sorted((ROOT / "crates").rglob("*.rs")):
        if path == ALLOWED or "target" in path.parts:
            continue
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if FORBIDDEN.search(line) or DECIMAL_ROUND.search(line):
                problems.append(f"{path.relative_to(ROOT)}:{number}: {line.strip()}")
    for manifest in sorted((ROOT / "crates").glob("*/Cargo.toml")):
        if manifest.parent.name != "money" and re.search(r"^fastnum\b", manifest.read_text(encoding="utf-8"), re.M):
            problems.append(f"{manifest.relative_to(ROOT)}: depends on the decimal library directly")
    for problem in problems:
        print(problem)
    if problems:
        print(f"{len(problems)} places round a decimal outside ic_money; use ic_money::round_integer or round_to_money")
        return 1
    print("only ic_money rounds decimals")
    return 0


if __name__ == "__main__":
    sys.exit(main())
