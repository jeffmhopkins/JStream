/**
 * A real `jstream-daemon --lan` for the tests that need one, seeded with a small
 * collection through its loopback listener and the control token. JSTREAM_DAEMON names
 * the daemon; without it those tests are skipped.
 */
import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { networkInterfaces, tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import WebSocket from "ws";

export const daemonPath = process.env.JSTREAM_DAEMON;
export const skip = daemonPath ? false : "set JSTREAM_DAEMON to a built jstream-daemon to run";

let daemon: ChildProcess | undefined;
let dir = "";
export let lanPort = 0;
let loopbackUrl = "";
export let fingerprint = "";
let control: WebSocket;
let nextId = 1;

function freePort(): Promise<number> {
	return new Promise((resolve) => {
		const server = createServer();
		server.listen(0, "127.0.0.1", () => {
			const port = (server.address() as { port: number }).port;
			server.close(() => resolve(port));
		});
	});
}

/** A call on the host's own loopback listener, with the control token. */
export function call(c: unknown): Promise<any> {
	const id = `t${nextId++}`;
	return new Promise((resolve, reject) => {
		const onMessage = (data: WebSocket.RawData) => {
			const m = JSON.parse(data.toString());
			if (m.id !== id) return;
			control.off("message", onMessage);
			if (m.type === "error") reject(new Error(JSON.stringify(m.error)));
			else resolve(m.result.result ?? m.result);
		};
		control.on("message", onMessage);
		control.send(JSON.stringify({ type: "request", id, call: c }));
	});
}

export async function until(what: string, check: () => boolean, ms = 4000): Promise<void> {
	const end = Date.now() + ms;
	while (!check()) {
		if (Date.now() > end) assert.fail(`timed out waiting for ${what}`);
		await new Promise((r) => setTimeout(r, 25));
	}
}

export async function startDaemon(): Promise<void> {
	if (!daemonPath) return;
	dir = mkdtempSync(join(tmpdir(), "jstream-sd-"));
	lanPort = await freePort();
	daemon = spawn(daemonPath, ["--token-file", join(dir, "token"), "--lan", "--lan-port", String(lanPort)], {
		stdio: ["pipe", "pipe", "ignore"],
	});
	const lines = createInterface({ input: daemon.stdout! });
	await new Promise<void>((resolve, reject) => {
		const timer = setTimeout(() => reject(new Error("the daemon didn't start")), 20000);
		lines.on("line", (line) => {
			const fp = line.match(/fingerprint \(SHA-256\): ([0-9A-F:]+)/);
			if (fp) fingerprint = fp[1];
			const url = line.match(/listening on (ws:\/\/\S+)/);
			if (url) {
				loopbackUrl = url[1];
				clearTimeout(timer);
				resolve();
			}
		});
	});
	const token = readFileSync(join(dir, "token"), "utf8").trim();
	control = new WebSocket(loopbackUrl, { headers: { Authorization: `Bearer ${token}` } });
	await new Promise<void>((resolve, reject) => {
		control.once("open", () => control.send(JSON.stringify({ type: "hello", protocol: { major: 0, minor: 169 }, client_name: "test-host" })));
		control.once("message", () => resolve());
		control.once("error", reject);
	});
	await call({
		kind: "batch",
		edits: [
			{ type: "scene.create", scene_id: "scn_a", name: "Intro" },
			{ type: "scene.create", scene_id: "scn_b", name: "Main" },
			{ type: "scene.create_variant", scene_id: "scn_b_wide", variant_of: "scn_b", name: "Main, wide" },
			{ type: "source.create", source_id: "src_bg", name: "Backdrop", kind: { type: "builtin", kind: "color" }, settings: { color: "#1e1e2eff", width: 64, height: 64 } },
			{ type: "item.add", scene_id: "scn_b", item_id: "itm_bg", source_id: "src_bg" },
			{ type: "filter.add", owner: { type: "source", source_id: "src_bg" }, chain: "video", filter_id: "flt_blur", kind: "blur", settings: {} },
			{ type: "source.create", source_id: "src_mic", name: "Mic", kind: { type: "builtin", kind: "audio_input" } },
			{ type: "timeline.set", scene_id: "scn_b", timeline: { timeline_id: "tl_loop", name: "Loop", duration_ms: 2000, loop: { range: { from_ms: 0, to_ms: 2000 } }, cues: [] } },
			{ type: "output.create", output_id: "out_rec", kind: { type: "record", path: join(dir, "show.mp4"), container: "mp4" } },
			{ type: "hotkey.set", chord: "Ctrl+Digit1", action: { type: "scene.switch", scene_id: "scn_a" } },
		],
	});
}

export function stopDaemon(): void {
	control?.close();
	daemon?.kill();
	if (dir) rmSync(dir, { recursive: true, force: true });
}

/** This machine's own network address: the network listener refuses loopback. */
export function lanHost(): string {
	const host = Object.values(networkInterfaces())
		.flat()
		.find((a) => a && a.family === "IPv4" && !a.internal)?.address;
	assert.ok(host, "this machine has no network address");
	return host;
}
