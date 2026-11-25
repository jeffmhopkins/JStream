/**
 * Stamps a JStream release version on the plugin's manifest, for the release workflow
 * (.github/workflows/release.yml, job `streamdeck`). Stream Deck wants four numbers, and a
 * later release must compare higher, so the pre-release part becomes the fourth number:
 * alpha.N is N, beta.N is 100 + N, rc.N is 200 + N, and a final release is 1000.
 * Run: node --experimental-strip-types scripts/set-version.ts 0.1.0-alpha.14
 */
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const MANIFEST = "io.github.jeffmhopkins.jstream.sdPlugin/manifest.json";
const STAGES: Record<string, number> = { alpha: 0, beta: 100, rc: 200 };
const VERSION_FIELD = /("Version":\s*")\d+\.\d+\.\d+\.\d+(")/;

/** The four-part Stream Deck version for a JStream version such as `0.1.0-beta.2`. */
export function streamDeckVersion(version: string): string {
	const m = /^(\d+)\.(\d+)\.(\d+)(?:-(alpha|beta|rc)\.(\d+))?$/.exec(version);
	if (!m) throw new Error(`not a JStream release version: ${version}`);
	const [, major, minor, patch, stage, n] = m;
	if (stage && Number(n) >= 100) throw new Error(`pre-release number too large: ${version}`);
	const build = stage ? STAGES[stage] + Number(n) : 1000;
	return [major, minor, patch, build].map(Number).join(".");
}

/** The manifest text with its plugin `Version` set (the first `Version` is the plugin's). */
export function stampManifest(text: string, version: string): string {
	if (!VERSION_FIELD.test(text)) throw new Error("no four-part plugin Version in the manifest");
	return text.replace(VERSION_FIELD, `$1${streamDeckVersion(version)}$2`);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
	const version = process.argv[2];
	if (!version) throw new Error("usage: node --experimental-strip-types scripts/set-version.ts <version>");
	writeFileSync(MANIFEST, stampManifest(readFileSync(MANIFEST, "utf8"), version));
	console.log(`${MANIFEST}: Version ${streamDeckVersion(version)}`);
}
