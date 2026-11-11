#!/usr/bin/env python3
"""Line coverage from an lcov file, with a floor.

Counts each source line once: a line is covered if any test ran it. The
project's coverage floor is NOT this figure: the floor is the stricter one in
the table `cargo llvm-cov` prints, which counts a line again for each generic
instantiation and enclosing closure. This script exists to list which lines
are uncovered; pass a floor of 0 to list without failing.

Usage: coverage_floor.py LCOV_FILE FLOOR_PERCENT
Prints the uncovered lines of every file, then the total. Exits 1 below the floor.
"""
import sys


def ranges(lines):
    out, start, prev = [], None, None
    for n in lines:
        if start is None:
            start = prev = n
        elif n == prev + 1:
            prev = n
        else:
            out.append((start, prev))
            start = prev = n
    if start is not None:
        out.append((start, prev))
    return ", ".join(str(a) if a == b else f"{a}-{b}" for a, b in out)


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    path, floor = sys.argv[1], float(sys.argv[2])
    files, current = {}, None
    with open(path) as lcov:
        for record in lcov:
            record = record.strip()
            if record.startswith("SF:"):
                current = files.setdefault(record[3:], {})
            elif record.startswith("DA:") and current is not None:
                line, count = record[3:].split(",")[:2]
                current[int(line)] = current.get(int(line), 0) + int(count)
    total = sum(len(lines) for lines in files.values())
    if total == 0:
        sys.exit(f"{path} records no lines")
    missed = 0
    for name in sorted(files):
        uncovered = sorted(n for n, count in files[name].items() if count == 0)
        missed += len(uncovered)
        if uncovered:
            short = name.split("/crates/")[-1]
            print(f"{short}: {len(uncovered)} uncovered: {ranges(uncovered)}")
    covered = 100.0 * (total - missed) / total
    print(f"line coverage {covered:.2f}% ({total - missed} of {total} lines; floor {floor:g}%)")
    if covered < floor:
        sys.exit(1)


if __name__ == "__main__":
    main()
