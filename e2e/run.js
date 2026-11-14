// Runs the persona end-to-end suite against a running stack.
//
//   node e2e/stack/stack.js &            # the whole system, seeded
//   node e2e/run.js                      # every test
//   ONLY=E2E-014 node e2e/run.js         # one test (or a comma separated list)
//   AREA=Claims node e2e/run.js          # one area
//   WORKERS=4 SHOTS=out/shots node e2e/run.js
//
// Writes `results.json`, `results.md` and `junit.xml` under `OUT` (default
// `e2e/.run/results`) and exits 1 if any test failed. A test that fails is
// run once more: if it passes then, it is reported as flaky, not as passed.
"use strict";
const fs = require("fs");
const path = require("path");
const { chromium } = require("playwright");
const { tests, Journey } = require("./lib/suite");

const root = path.resolve(__dirname, "..");
const runDir = path.resolve(root, process.env.IC_E2E_RUN || "e2e/.run");
const out = path.resolve(process.env.OUT || path.join(runDir, "results"));

for (const file of fs.readdirSync(path.join(__dirname, "tests")).filter((f) => f.endsWith(".js")).sort()) require("./tests/" + file);

async function main() {
  const stack = JSON.parse(fs.readFileSync(path.join(runDir, "stack.json"), "utf8"));
  const only = process.env.ONLY ? process.env.ONLY.split(",") : null;
  const area = process.env.AREA;
  const chosen = tests.filter((t) => (!only || only.includes(t.meta.id)) && (!area || t.meta.area.toLowerCase() === area.toLowerCase()));
  if (process.argv.includes("--list")) { for (const t of chosen) console.log(`${t.meta.id}\t${t.meta.area}\t${t.meta.persona}\t${t.meta.title}`); return 0; }
  const options = { run: Date.now().toString(36).slice(-5), shots: process.env.SHOTS || path.join(out, "shots"), video: process.env.VIDEO, theme: process.env.THEME, width: Number(process.env.WIDTH) || 1280 };
  const browser = await chromium.launch();
  const results = [];
  const held = new Set();
  const queue = [...chosen];
  const started = Date.now();

  async function attempt(t, nth = 1) {
    const journey = new Journey(t.meta, browser, stack, { ...options, attempt: nth });
    const began = Date.now();
    let error = null;
    try {
      await Promise.race([t.run(journey), new Promise((_, no) => setTimeout(() => no(new Error("the test ran past its limit of 120 seconds")), 120000))]);
    } catch (e) { error = String(e.message || e).split("\n").slice(0, 4).join(" / "); }
    const problems = [...(error ? [error] : []), ...journey.findings()];
    await journey.close(problems.length > 0);
    return { problems, ms: Date.now() - began };
  }
  async function worker() {
    for (;;) {
      const at = queue.findIndex((t) => !t.meta.lock || !held.has(t.meta.lock));
      if (at < 0) { if (!queue.length) return; await new Promise((r) => setTimeout(r, 200)); continue; }
      const [t] = queue.splice(at, 1);
      if (t.meta.lock) held.add(t.meta.lock);
      let r = await attempt(t), flaky = false;
      const first = r.problems;
      if (r.problems.length && process.env.RETRY !== "0") { const again = await attempt(t, 2); if (!again.problems.length) { flaky = true; r = { problems: [], ms: r.ms + again.ms }; } }
      if (t.meta.lock) held.delete(t.meta.lock);
      const status = r.problems.length ? "failed" : flaky ? "flaky" : "passed";
      results.push({ ...t.meta, status, ms: r.ms, problems: r.problems, first_attempt: flaky ? first : undefined });
      console.log(`${status === "passed" ? "ok   " : status === "flaky" ? "FLAKY" : "FAIL "} ${t.meta.id} ${t.meta.persona} ${t.meta.title}${r.problems.length ? "\n        " + r.problems.join("\n        ") : ""}`);
    }
  }
  await Promise.all(Array.from({ length: Number(process.env.WORKERS) || 4 }, worker));
  await browser.close();

  results.sort((a, b) => a.id.localeCompare(b.id, undefined, { numeric: true }));
  const count = (s) => results.filter((r) => r.status === s).length;
  const summary = { run: options.run, at: new Date().toISOString(), seconds: Math.round((Date.now() - started) / 1000), total: results.length, passed: count("passed"), flaky: count("flaky"), failed: count("failed") };
  fs.mkdirSync(out, { recursive: true });
  fs.writeFileSync(path.join(out, "results.json"), JSON.stringify({ summary, results }, null, 2));
  const areas = [...new Set(results.map((r) => r.area))];
  const md = [`# Persona end-to-end run ${summary.at}`, "", `${summary.total} tests in ${summary.seconds} seconds: ${summary.passed} passed, ${summary.flaky} flaky, ${summary.failed} failed.`, "",
    "| Area | Tests | Passed | Flaky | Failed |", "| --- | --- | --- | --- | --- |",
    ...areas.map((a) => { const rs = results.filter((r) => r.area === a); return `| ${a} | ${rs.length} | ${rs.filter((r) => r.status === "passed").length} | ${rs.filter((r) => r.status === "flaky").length} | ${rs.filter((r) => r.status === "failed").length} |`; }),
    "", ...(summary.failed + summary.flaky ? ["## Not passed", "", ...results.filter((r) => r.status !== "passed").flatMap((r) => [`### ${r.id} ${r.title} (${r.status})`, "", `Persona ${r.persona}, story ${r.story}${r.matrix.length ? ", matrix " + r.matrix.join(" ") : ""}.`, "", ...(r.problems.length ? r.problems : r.first_attempt || []).map((p) => `- ${p}`), ""])] : [])].join("\n");
  fs.writeFileSync(path.join(out, "results.md"), md + "\n");
  const esc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/"/g, "&quot;");
  fs.writeFileSync(path.join(out, "junit.xml"), `<?xml version="1.0" encoding="UTF-8"?>\n<testsuite name="persona-e2e" tests="${summary.total}" failures="${summary.failed}" time="${summary.seconds}">\n${results.map((r) => `  <testcase classname="${esc(r.area)}" name="${esc(r.id + " " + r.title)}" time="${(r.ms / 1000).toFixed(1)}">${r.status === "failed" ? `<failure message="${esc(r.problems[0])}">${esc(r.problems.join("\n"))}</failure>` : ""}</testcase>`).join("\n")}\n</testsuite>\n`);
  console.log(`\n${summary.total} tests: ${summary.passed} passed, ${summary.flaky} flaky, ${summary.failed} failed, in ${summary.seconds}s. Report: ${path.join(out, "results.md")}`);
  return summary.failed ? 1 : 0;
}

main().then((code) => process.exit(code)).catch((e) => { console.error(e); process.exit(2); });
