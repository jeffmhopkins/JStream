/** The release version stamped on the manifest (scripts/set-version.ts). */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { stampManifest, streamDeckVersion } from "../scripts/set-version.ts";

test("pre-releases map into the fourth number and order below the final release", () => {
	assert.equal(streamDeckVersion("0.1.0-alpha.14"), "0.1.0.14");
	assert.equal(streamDeckVersion("0.1.0-beta.1"), "0.1.0.101");
	assert.equal(streamDeckVersion("0.1.0-rc.2"), "0.1.0.202");
	assert.equal(streamDeckVersion("0.1.0"), "0.1.0.1000");
	assert.equal(streamDeckVersion("1.20.3"), "1.20.3.1000");
});

test("versions it can't order are refused", () => {
	for (const bad of ["v0.1.0", "0.1", "0.1.0-alpha", "0.1.0-dev.1", "0.1.0-alpha.100", "0.1.0+build"]) {
		assert.throws(() => streamDeckVersion(bad), Error, bad);
	}
});

test("the stamp sets the plugin's Version and leaves the Node runtime's alone", () => {
	const manifest = readFileSync("io.github.jeffmhopkins.jstream.sdPlugin/manifest.json", "utf8");
	const stamped = stampManifest(manifest, "0.1.0-alpha.14");
	assert.equal(JSON.parse(stamped).Version, "0.1.0.14");
	assert.equal(JSON.parse(stamped).Nodejs.Version, JSON.parse(manifest).Nodejs.Version);
	assert.equal(stamped.replace('"Version": "0.1.0.14"', `"Version": "${JSON.parse(manifest).Version}"`), manifest);
});
