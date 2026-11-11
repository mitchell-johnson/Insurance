#!/usr/bin/env python3
"""Every test says what it is for and why it is worth having.

A test function must be preceded by a doc comment with a `Purpose:` line and
a `Why it matters:` line. This script lists the tests that are not, and
exits non-zero if there are any. CI runs it.

    python3 tools/check_test_docs.py [crates]
"""

import pathlib
import re
import sys

TEST_ATTRIBUTE = re.compile(r"^\s*#\[(tokio::)?test(\(.*\))?\]\s*$")
PROPTEST_FN = re.compile(r"^\s*fn\s+\w+\s*\(.*\bin\b")
FN_NAME = re.compile(r"\bfn\s+(\w+)")


def documented(lines, at):
    """Whether the doc comment ending just above line `at` has both parts."""
    doc = []
    i = at - 1
    while i >= 0:
        text = lines[i].strip()
        if text.startswith("///"):
            doc.append(text[3:].strip())
        elif text.startswith("#[") or text.startswith("#!["):
            pass
        else:
            break
        i -= 1
    joined = " ".join(reversed(doc))
    return "Purpose:" in joined and "Why it matters:" in joined


def undocumented(path):
    lines = path.read_text(encoding="utf-8").splitlines()
    in_proptest = False
    for n, line in enumerate(lines):
        if line.startswith("proptest!"):
            in_proptest = True
        elif in_proptest and line.startswith("}"):
            in_proptest = False
        is_test = TEST_ATTRIBUTE.match(line) or (in_proptest and PROPTEST_FN.match(line) and "#[test]" not in lines[n - 1])
        if not is_test:
            continue
        fn_line = next((k for k in range(n, min(n + 6, len(lines))) if FN_NAME.search(lines[k])), n)
        if not documented(lines, n):
            name = FN_NAME.search(lines[fn_line])
            yield fn_line + 1, name.group(1) if name else "?"


def main():
    root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "crates")
    missing = []
    tests = 0
    for path in sorted(root.rglob("*.rs")):
        if "target" in path.parts:
            continue
        text = path.read_text(encoding="utf-8")
        tests += len(re.findall(r"^\s*#\[(?:tokio::)?test(?:\(.*\))?\]\s*$", text, flags=re.M))
        missing.extend((path, line, name) for line, name in undocumented(path))
    for path, line, name in missing:
        print(f"{path}:{line}: test `{name}` has no `Purpose:` and `Why it matters:` doc comment")
    print(f"{tests} tests checked, {len(missing)} without a documented purpose")
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
