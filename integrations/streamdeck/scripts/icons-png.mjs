// Renders the plugin icon (imgs/plugin/marketplace.svg) to the PNGs Stream Deck's
// preferences need: 256 × 256 and 512 × 512 (@2x). Uses Playwright's Chromium.
// Run: node scripts/icons-png.mjs
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

// Playwright is not a dependency of the plugin: run with NODE_PATH pointing at a global install.
const { chromium } = createRequire(import.meta.url)("playwright");

const dir = "io.github.jeffmhopkins.jstream.sdPlugin/imgs/plugin";
const svg = readFileSync(`${dir}/marketplace.svg`, "utf8");
const browser = await chromium.launch();
for (const [scale, name] of [[1, "marketplace.png"], [2, "marketplace@2x.png"]]) {
	const page = await browser.newPage({ viewport: { width: 256, height: 256 }, deviceScaleFactor: scale });
	await page.setContent(`<html><body style="margin:0;background:transparent">${svg}</body></html>`);
	await page.locator("svg").screenshot({ path: `${dir}/${name}`, omitBackground: true });
}
await browser.close();
