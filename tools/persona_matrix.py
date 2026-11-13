#!/usr/bin/env python3
"""Validate the persona test matrix and generate its CSV and coverage report.

Sources (hand-written, in docs/testing/personas/):
  personas-insurance.md   persona, situation and cast tables
  user-stories.md         stories with IDs of the form US-<persona>-<n>
  test-matrix-*.md        test cases, one table per capability area
  traceability.md         which built tests (under crates/) carry out each test case

Outputs (generated, same folder):
  test-matrix.csv         every test case as one row
  coverage.md             counts and cross-references

Run:  python3 tools/persona_matrix.py          (writes outputs)
      python3 tools/persona_matrix.py --check  (validates only, for CI)

Exits non-zero if any cross-reference is broken or anything is uncovered.
"""
from __future__ import annotations

import csv
import re
import sys
from collections import defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
ROOT = REPO / "docs" / "testing" / "personas"
CRATES = REPO / "crates"
TYPES = {
    "J": "Scripted journey",
    "N": "Negative or refusal",
    "P": "Property test",
    "S": "Simulation invariant",
    "R": "Reconciliation",
    "X": "Access and privacy",
    "F": "Fault injection",
    "L": "Load",
    "O": "Operational",
}
PHASES = ["A", "B", "C", "D"]
ID_RE = re.compile(r"^(P|B|S|D|R|E|N|ST|TPA|PR|FA|BK)(\d+)$")


def cells(line: str) -> list[str]:
    return [c.strip() for c in line.strip().strip("|").split("|")]


def natural(key: str):
    m = re.match(r"^([A-Z]+)-?(\d+)", key)
    return (m.group(1), int(m.group(2))) if m else (key, 0)


def load_personas() -> dict[str, str]:
    personas: dict[str, str] = {}
    for line in (ROOT / "personas-insurance.md").read_text().splitlines():
        if not line.startswith("|"):
            continue
        row = cells(line)
        if ID_RE.match(row[0]):
            if row[0] in personas:
                raise SystemExit(f"duplicate persona id {row[0]}")
            personas[row[0]] = row[1]
    return personas


def load_stories() -> dict[str, dict]:
    stories: dict[str, dict] = {}
    persona = None
    for line in (ROOT / "user-stories.md").read_text().splitlines():
        h = re.match(r"^### (\S+) ", line)
        if h:
            persona = h.group(1)
            continue
        m = re.match(r"^- \*\*(US-[A-Z0-9]+-\d+)\*\* (.*)$", line)
        if m:
            sid, text = m.group(1), m.group(2)
            if sid in stories:
                raise SystemExit(f"duplicate story id {sid}")
            stories[sid] = {"persona": persona, "text": text, "gap": "(design gap)" in text}
    return stories


def load_tests() -> list[dict]:
    tests: list[dict] = []
    for path in sorted(ROOT.glob("test-matrix-*.md")):
        area = title = design = None
        for line in path.read_text().splitlines():
            h = re.match(r"^## ([A-Z]+) (.*)$", line)
            if h:
                area, title, design = h.group(1), h.group(2), ""
                continue
            if line.startswith("Design:"):
                design = line[len("Design:"):].strip().rstrip(".")
                continue
            if not line.startswith("|") or area is None:
                continue
            row = cells(line)
            if len(row) != 8 or not re.match(r"^[A-Z]+-\d{3}$", row[0]):
                continue
            tests.append({
                "id": row[0], "area": area, "area_title": title, "design_refs": design,
                "story": row[1], "personas": [p.strip() for p in row[2].split(",")],
                "scenario": row[3], "expected": row[4], "type": row[5],
                "phase": row[6], "priority": row[7], "file": path.name,
            })
    return tests


def load_trace() -> dict[str, dict]:
    trace: dict[str, dict] = {}
    path = ROOT / "traceability.md"
    if not path.exists():
        return trace
    for line in path.read_text().splitlines():
        if not line.startswith("|"):
            continue
        row = cells(line)
        if len(row) != 3 or not re.match(r"^[A-Z]+-\d{3}$", row[0]):
            continue
        refs = [r.strip().strip("`") for r in row[2].split("<br>") if r.strip()]
        if row[0] in trace:
            raise SystemExit(f"duplicate traceability row {row[0]}")
        trace[row[0]] = {"status": row[1], "tests": refs}
    return trace


def validate_trace(tests, trace) -> list[str]:
    errors: list[str] = []
    known = {t["id"] for t in tests}
    sources: dict[Path, str] = {}
    for tid, t in trace.items():
        if tid not in known:
            errors.append(f"traceability {tid}: not a test in the matrix")
        if t["status"] not in {"full", "part"}:
            errors.append(f"traceability {tid}: status must be full or part, not {t['status']}")
        if not t["tests"]:
            errors.append(f"traceability {tid}: names no built test")
        for ref in t["tests"]:
            if "::" not in ref:
                errors.append(f"traceability {tid}: {ref} is not file::test")
                continue
            file, name = ref.split("::", 1)
            src = CRATES / file
            if src not in sources:
                sources[src] = src.read_text() if src.is_file() else ""
            if not re.search(rf"\bfn\s+{re.escape(name)}\b", sources[src]):
                errors.append(f"traceability {tid}: no test {name} in crates/{file}")
    return errors


def validate(personas, stories, tests) -> list[str]:
    errors: list[str] = []
    seen = set()
    story_tests = defaultdict(list)
    persona_tests = defaultdict(list)
    situations = defaultdict(list)
    for t in tests:
        tid = t["id"]
        if tid in seen:
            errors.append(f"{tid}: duplicate test id")
        seen.add(tid)
        if not tid.startswith(t["area"] + "-"):
            errors.append(f"{tid}: id does not match area {t['area']}")
        if t["story"] not in stories:
            errors.append(f"{tid}: unknown story {t['story']}")
        else:
            story_tests[t["story"]].append(tid)
            persona_tests[stories[t["story"]]["persona"]].append(tid)
        for p in t["personas"]:
            if p == "all":
                continue
            if p not in personas:
                errors.append(f"{tid}: unknown persona {p}")
            else:
                persona_tests[p].append(tid)
        if t["type"] not in TYPES:
            errors.append(f"{tid}: unknown type {t['type']}")
        if t["phase"] not in PHASES:
            errors.append(f"{tid}: unknown phase {t['phase']}")
        if t["priority"] not in {"1", "2", "3"}:
            errors.append(f"{tid}: unknown priority {t['priority']}")
        for s in set(re.findall(r"\bS(\d+)\b", t["scenario"])):
            situations[f"S{s}"].append(tid)
    for sid, s in stories.items():
        if s["persona"] not in personas:
            errors.append(f"{sid}: story heading names unknown persona {s['persona']}")
        if not story_tests[sid]:
            errors.append(f"{sid}: story has no test")
    for pid in personas:
        if pid.startswith("S") and not pid.startswith("ST"):
            if not situations[pid]:
                errors.append(f"{pid}: situation is not named in any test scenario")
        elif not persona_tests[pid]:
            errors.append(f"{pid}: persona has no test")
        if not (pid.startswith("S") and not pid.startswith("ST")):
            if not any(s["persona"] == pid for s in stories.values()):
                errors.append(f"{pid}: persona has no user story")
    return errors, story_tests, persona_tests, situations


def write_csv(tests, trace):
    with (ROOT / "test-matrix.csv").open("w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["id", "area", "area_title", "design_sections", "story", "personas",
                    "scenario", "expected", "type", "phase", "priority", "built", "built_tests"])
        for t in tests:
            w.writerow([t["id"], t["area"], t["area_title"], t["design_refs"], t["story"],
                        " ".join(t["personas"]), t["scenario"], t["expected"],
                        TYPES[t["type"]], t["phase"], t["priority"],
                        trace.get(t["id"], {}).get("status", "none"),
                        " ".join(trace.get(t["id"], {}).get("tests", []))])


def write_coverage(personas, stories, tests, story_tests, persona_tests, situations, trace):
    out: list[str] = []
    add = out.append
    add("# Coverage report")
    add("")
    add("Generated by `tools/persona_matrix.py` from the stories and test matrix. Do not edit by hand.")
    add("")
    people = [p for p in personas if not (p.startswith("S") and not p.startswith("ST"))]
    sits = [p for p in personas if p.startswith("S") and not p.startswith("ST")]
    add(f"- Personas and cast members: {len(people)}")
    add(f"- Situations: {len(sits)}")
    add(f"- User stories: {len(stories)}")
    add(f"- Test cases: {len(tests)}")
    add("")

    add("## Tests by area and build phase")
    add("")
    add("| Area | Design sections | " + " | ".join(f"Phase {p}" for p in PHASES) + " | Total | Priority 1 |")
    add("| --- | --- | " + " | ".join("---:" for _ in PHASES) + " | ---: | ---: |")
    areas: dict[str, dict] = {}
    for t in tests:
        a = areas.setdefault(t["area"], {"title": t["area_title"], "design": t["design_refs"],
                                         "n": defaultdict(int), "p1": 0})
        a["n"][t["phase"]] += 1
        a["p1"] += t["priority"] == "1"
    for code, a in areas.items():
        total = sum(a["n"].values())
        add(f"| {code} {a['title']} | {a['design']} | "
            + " | ".join(str(a["n"][p]) for p in PHASES) + f" | {total} | {a['p1']} |")
    tot = defaultdict(int)
    for t in tests:
        tot[t["phase"]] += 1
    add("| **Total** | | " + " | ".join(f"**{tot[p]}**" for p in PHASES)
        + f" | **{len(tests)}** | **{sum(t['priority'] == '1' for t in tests)}** |")
    add("")

    def built(t):
        return trace.get(t["id"], {}).get("status", "none")

    add("## Built so far")
    add("")
    add("How many persona tests the built test suite carries out, from `traceability.md`. "
        "**Full** means the built tests check what the row expects; **part** means some of it.")
    add("")
    add("| Area | Full | Part | None | Priority 1 not full |")
    add("| --- | ---: | ---: | ---: | ---: |")
    for code, a in areas.items():
        mine = [t for t in tests if t["area"] == code]
        n = {k: sum(built(t) == k for t in mine) for k in ("full", "part", "none")}
        gap = sum(t["priority"] == "1" and built(t) != "full" for t in mine)
        add(f"| {code} {a['title']} | {n['full']} | {n['part']} | {n['none']} | {gap} |")
    n = {k: sum(built(t) == k for t in tests) for k in ("full", "part", "none")}
    add(f"| **Total** | **{n['full']}** | **{n['part']}** | **{n['none']}** | "
        f"**{sum(t['priority'] == '1' and built(t) != 'full' for t in tests)}** |")
    add("")
    add("| Phase | Full | Part | None |")
    add("| --- | ---: | ---: | ---: |")
    for ph in PHASES:
        mine = [t for t in tests if t["phase"] == ph]
        add(f"| {ph} | " + " | ".join(str(sum(built(t) == k for t in mine)) for k in ("full", "part", "none")) + " |")
    add("")
    add("### Priority 1 tests with no built test, by area")
    add("")
    for code, a in areas.items():
        ids = [t["id"] for t in tests if t["area"] == code and t["priority"] == "1" and built(t) == "none"]
        if ids:
            add(f"- **{code}** ({len(ids)}): {', '.join(ids)}")
    add("")
    add("### Priority 1 tests only partly built, by area")
    add("")
    for code, a in areas.items():
        ids = [t["id"] for t in tests if t["area"] == code and t["priority"] == "1" and built(t) == "part"]
        if ids:
            add(f"- **{code}** ({len(ids)}): {', '.join(ids)}")
    add("")

    add("## Tests by type")
    add("")
    add("| Type | Count |")
    add("| --- | ---: |")
    for k, name in TYPES.items():
        add(f"| {name} | {sum(t['type'] == k for t in tests)} |")
    add("")

    add("## Design gaps the personas expose")
    add("")
    for sid, s in stories.items():
        if s["gap"]:
            add(f"- {sid} ({s['persona']} {personas[s['persona']]}): tests {', '.join(story_tests[sid])}")
    add("")

    add("## Situations")
    add("")
    add("| Situation | Tests |")
    add("| --- | --- |")
    for sid in sorted(sits, key=natural):
        add(f"| {sid} {personas[sid]} | {', '.join(situations[sid])} |")
    add("")

    add("## Personas")
    add("")
    add("| Persona | Stories | Tests | Test IDs |")
    add("| --- | ---: | ---: | --- |")
    for pid in people:
        ids = sorted(set(persona_tests[pid]), key=natural)
        n_st = sum(s["persona"] == pid for s in stories.values())
        add(f"| {pid} {personas[pid]} | {n_st} | {len(ids)} | {', '.join(ids)} |")
    add("")

    add("## Stories")
    add("")
    add("| Story | Tests |")
    add("| --- | --- |")
    for sid in stories:
        add(f"| {sid} | {', '.join(story_tests[sid])} |")
    add("")
    (ROOT / "coverage.md").write_text("\n".join(out))


def main() -> int:
    personas, stories, tests, trace = load_personas(), load_stories(), load_tests(), load_trace()
    errors, story_tests, persona_tests, situations = validate(personas, stories, tests)
    errors += validate_trace(tests, trace)
    for e in errors:
        print("ERROR", e)
    full = sum(t["status"] == "full" for t in trace.values())
    print(f"{len(personas)} personas and situations, {len(stories)} stories, {len(tests)} tests "
          f"({full} fully and {len(trace) - full} partly built), {len(errors)} errors")
    if "--check" not in sys.argv:
        write_csv(tests, trace)
        write_coverage(personas, stories, tests, story_tests, persona_tests, situations, trace)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
