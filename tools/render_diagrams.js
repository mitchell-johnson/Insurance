// Renders each diagram source under docs/ (an .html file in docs/architecture/
// or docs/integration/) to a PNG of the same name beside it, at twice the
// pixel density so it stays sharp in a rendered Markdown page.
//
//   node tools/render_diagrams.js            # every diagram
//   node tools/render_diagrams.js docs/architecture/system-overview.html
//
// Needs Playwright with Chromium (the e2e suite's dependency: `npm ci` in e2e/,
// then run with NODE_PATH=e2e/node_modules). Edit the HTML, never the PNG, and
// commit both.
const path = require("path");
const fs = require("fs");
const { chromium } = require("playwright");

const root = path.resolve(__dirname, "..");
const dirs = ["docs/architecture", "docs/integration"];

function sources(args) {
  if (args.length) return args.map((a) => path.resolve(a));
  return dirs.flatMap((d) => {
    const full = path.join(root, d);
    if (!fs.existsSync(full)) return [];
    return fs.readdirSync(full).filter((f) => f.endsWith(".html")).map((f) => path.join(full, f));
  });
}

(async () => {
  const browser = await chromium.launch();
  try {
    for (const src of sources(process.argv.slice(2))) {
      const page = await browser.newPage({ viewport: { width: 1640, height: 800 }, deviceScaleFactor: 2 });
      await page.goto("file://" + src);
      const out = src.replace(/\.html$/, ".png");
      await page.locator(".page").screenshot({ path: out });
      await page.close();
      console.log(path.relative(root, out));
    }
  } finally {
    await browser.close();
  }
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
