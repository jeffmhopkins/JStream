// The property inspector shared by every JStream key: the target picked from JStream's
// live lists, and the connection (find, pair, forget), which every key shares. It talks
// to Stream Deck over the socket Stream Deck opens for it, and to the plugin with
// sendToPlugin; the plugin answers with sendToPropertyInspector.
"use strict";

const PLUGIN = "io.github.jeffmhopkins.jstream";

/** Each key's fields. `list` names a list the plugin sends; `modes` the labels for toggle, on, off. */
const FORMS = {
	show: [
		{
			setting: "command",
			label: "Press",
			options: [
				{ value: "restart", label: "Restart" },
				{ value: "play", label: "Play / Pause / Continue" },
				{ value: "skip", label: "Next" },
				{ value: "back", label: "Prev" },
				{ value: "stop", label: "Stop" },
				{ value: "go_to", label: "Go to bookmark" },
				{ value: "number", label: "Go to bookmark number" },
				{ value: "continue", label: "Continue only" },
				{ value: "pause", label: "Pause only" },
			],
		},
		{ setting: "show_id", label: "Show", list: "shows", when: (s) => ["play", "restart", "go_to", undefined].includes(s.command) },
		{ setting: "show_id", label: "Show", list: "shows", blank: "The one running", when: (s) => ["continue", "pause", "stop"].includes(s.command) },
		{ setting: "bookmark_id", label: "Bookmark", list: "bookmarks", by: "show_id", when: (s) => s.command === "go_to" },
		{
			setting: "number",
			label: "Number",
			options: [1, 2, 3, 4, 5, 6, 7, 8].map((n) => ({ value: String(n), label: String(n) })),
			when: (s) => s.command === "number",
		},
		{
			setting: "rehearse",
			label: "Where",
			options: [
				{ value: "no", label: "On program" },
				{ value: "yes", label: "Rehearse on preview" },
			],
			when: (s) => ["play", "restart"].includes(s.command ?? "play"),
		},
	],
	scene: [
		{ setting: "scene_id", label: "Scene", list: "scenes" },
		{ setting: "keyframe_id", label: "Keyframe", list: "sceneKeyframes", by: "scene_id", blank: "None" },
		{ setting: "preset_id", label: "Transition", list: "transitions", blank: "Default" },
	],
	keyframe: [
		{ setting: "keyframe_id", label: "Keyframe", list: "keyframes" },
		{ setting: "preset_id", label: "Transition", list: "transitions", blank: "Default" },
	],
	visibility: [
		{ setting: "scene_id", label: "Scene", list: "scenes" },
		{ setting: "item_id", label: "Source", list: "items", by: "scene_id" },
		{ setting: "mode", label: "Press", modes: ["Toggle", "Show", "Hide"] },
	],
	mute: [
		{ setting: "source_id", label: "Source", list: "sources" },
		{ setting: "mode", label: "Press", modes: ["Toggle", "Mute", "Unmute"] },
	],
	sequence: [
		{ setting: "timeline_id", label: "Sequence", list: "sequences" },
		{ setting: "mode", label: "Press", modes: ["Toggle", "Play", "Stop"] },
	],
	effect: [
		{ setting: "effect", label: "Effect", list: "effects" },
		{ setting: "mode", label: "Press", modes: ["Toggle", "On", "Off"] },
	],
	output: [
		{ setting: "output_id", label: "Output", list: "outputs" },
		{ setting: "mode", label: "Press", modes: ["Toggle", "Start", "Stop"] },
	],
	channel: [
		{ setting: "channel_id", label: "Channel", list: "channels" },
		{ setting: "mode", label: "Press", modes: ["Toggle", "On", "Off"] },
	],
	action: [
		{ setting: "chord", label: "Hotkey", list: "hotkeys", blank: "None" },
		{ setting: "custom", label: "Or JSON", textarea: '{"type": "output.start", "output_id": "out_1"}' },
	],
};

let socket;
let uuid;
let actionUuid;
let settings = {};
let lists = {};
let offered;

const $ = (id) => document.getElementById(id);

window.connectElgatoStreamDeckSocket = (port, inUuid, registerEvent, _info, actionInfo) => {
	uuid = inUuid;
	const info = JSON.parse(actionInfo);
	actionUuid = info.action;
	settings = info.payload.settings || {};
	socket = new WebSocket(`ws://127.0.0.1:${port}`);
	socket.onopen = () => {
		socket.send(JSON.stringify({ event: registerEvent, uuid }));
		toPlugin({ type: "hello" });
	};
	socket.onmessage = (msg) => {
		const data = JSON.parse(msg.data);
		if (data.event === "sendToPropertyInspector") {
			fromPlugin(data.payload);
		} else if (data.event === "didReceiveSettings" && data.context === uuid) {
			settings = data.payload.settings || {};
			drawTarget();
		}
	};
	drawTarget();
};

function toPlugin(payload) {
	socket.send(JSON.stringify({ event: "sendToPlugin", action: actionUuid, context: uuid, payload }));
}

function save() {
	socket.send(JSON.stringify({ event: "setSettings", context: uuid, payload: settings }));
}

function fromPlugin(message) {
	switch (message.type) {
		case "connection":
			drawConnection(message);
			break;
		case "lists":
			lists = message.lists || {};
			drawTarget();
			break;
		case "discovered":
			drawFound(message.found || []);
			break;
		case "probed":
			offered = message;
			$("offered").textContent = message.fingerprint;
			$("confirm").hidden = false;
			$("probe").disabled = false;
			$("pair").disabled = false;
			say("");
			$("code").focus();
			break;
		case "paired":
			$("confirm").hidden = true;
			$("code").value = "";
			offered = undefined;
			say("Paired.", true);
			break;
		case "failed":
			$("probe").disabled = false;
			$("pair").disabled = false;
			say(message.message);
			break;
	}
}

// ---- The target ----

function drawTarget() {
	const kind = (actionUuid || "").replace(`${PLUGIN}.`, "");
	const form = FORMS[kind];
	const target = $("target");
	if (!form) {
		target.hidden = true;
		return;
	}
	const focused = document.activeElement && document.activeElement.dataset.setting;
	target.replaceChildren();
	for (const field of form) {
		if (field.when && !field.when(settings)) continue;
		const row = document.createElement("div");
		row.className = "row";
		const label = document.createElement("label");
		label.textContent = field.label;
		row.append(label);
		row.append(field.textarea !== undefined ? textarea(field) : select(field));
		target.append(row);
	}
	target.hidden = false;
	if (focused) {
		const again = target.querySelector(`[data-setting="${focused}"]`);
		if (again && again.tagName === "TEXTAREA") again.focus();
	}
}

function select(field) {
	const el = document.createElement("select");
	el.dataset.setting = field.setting;
	const current = settings[field.setting] ?? "";
	let options;
	if (field.options) {
		options = field.options;
	} else if (field.modes) {
		options = [
			{ value: "toggle", label: field.modes[0] },
			{ value: "on", label: field.modes[1] },
			{ value: "off", label: field.modes[2] },
		];
	} else {
		let list = lists[field.list];
		if (field.by) list = (list || {})[settings[field.by]] || [];
		options = list || [];
		if (field.blank !== undefined) {
			options = [{ value: "", label: field.blank }, ...options];
		} else if (!current) {
			options = [{ value: "", label: options.length ? "Pick one" : "Not connected" }, ...options];
		}
	}
	if (current && !options.some((o) => o.value === current)) {
		// Keeps a saved target that the lists don't have now (offline, or deleted).
		options = [{ value: current, label: lists[field.list] ? `${current} (missing)` : `${current}` }, ...options];
	}
	const groups = new Map();
	for (const o of options) {
		const opt = document.createElement("option");
		opt.value = o.value;
		opt.textContent = o.label;
		if (o.group) {
			let group = groups.get(o.group);
			if (!group) {
				group = document.createElement("optgroup");
				group.label = o.group;
				groups.set(o.group, group);
				el.append(group);
			}
			group.append(opt);
		} else {
			el.append(opt);
		}
	}
	el.value = field.modes ? current || "toggle" : field.options ? current || field.options[0].value : current;
	el.onchange = () => {
		settings[field.setting] = el.value || undefined;
		if (field.setting === "scene_id" && FORMS.visibility === currentForm()) {
			settings.item_id = undefined;
		}
		if (field.setting === "show_id") {
			settings.bookmark_id = undefined;
		}
		save();
		drawTarget();
	};
	return el;
}

function textarea(field) {
	const el = document.createElement("textarea");
	el.dataset.setting = field.setting;
	el.placeholder = field.textarea;
	el.spellcheck = false;
	el.className = "mono small";
	el.value = settings[field.setting] || "";
	el.onchange = () => {
		settings[field.setting] = el.value.trim() || undefined;
		save();
	};
	return el;
}

function currentForm() {
	return FORMS[(actionUuid || "").replace(`${PLUGIN}.`, "")];
}

// ---- The connection ----

function drawConnection(c) {
	const dot = $("dot");
	const state = c.status.state;
	dot.className = `dot ${state === "connected" ? "connected" : state === "error" ? "error" : state === "connecting" ? "connecting" : ""}`;
	let summary;
	switch (state) {
		case "connected":
			summary = `Connected to JStream at ${c.host}`;
			break;
		case "connecting":
			summary = `Connecting to ${c.host}…`;
			break;
		case "error":
			summary = `Not connected: ${c.status.message}`;
			break;
		default:
			summary = "Not paired with JStream";
	}
	$("summary").textContent = summary;
	$("paired").hidden = !c.paired;
	$("unpaired").hidden = c.paired;
	$("paired-address").textContent = `${c.host}:${c.port}`;
	$("paired-fingerprint").textContent = c.fingerprint;
	if (!c.paired) {
		if (!$("host").value) $("host").value = c.host || "";
		if (!$("device-name").value) $("device-name").value = c.deviceName;
		$("connection").open = true;
	} else if (state !== "connected") {
		$("connection").open = true;
	}
}

function drawFound(found) {
	const box = $("found");
	box.replaceChildren();
	$("find").disabled = false;
	$("find").textContent = "Find JStream";
	if (!found.length) {
		const none = document.createElement("div");
		none.className = "none";
		none.textContent = "None found. Is announcing on in JStream's Settings? You can type the address instead.";
		box.append(none);
		return;
	}
	for (const f of found) {
		const b = document.createElement("button");
		b.textContent = `${f.name} (${f.host}:${f.port})`;
		b.onclick = () => {
			$("host").value = f.host;
			$("port").value = f.port;
			probe();
		};
		box.append(b);
	}
}

function probe() {
	const host = $("host").value.trim();
	if (!host) {
		say("Type JStream's address, or find it.");
		return;
	}
	$("probe").disabled = true;
	$("confirm").hidden = true;
	say("");
	toPlugin({ type: "probe", host, port: Number($("port").value) || 4456 });
}

function say(text, ok) {
	const m = $("message");
	m.textContent = text;
	m.className = ok ? "message ok" : "message";
	m.hidden = !text;
}

$("find").onclick = () => {
	$("find").disabled = true;
	$("find").textContent = "Looking…";
	$("found").replaceChildren();
	toPlugin({ type: "discover" });
};
$("probe").onclick = probe;
$("host").onkeydown = (e) => {
	if (e.key === "Enter") probe();
};
$("pair").onclick = () => {
	if (!offered) return;
	const code = $("code").value.trim();
	if (!/^[0-9A-Za-z]{4}-?[0-9A-Za-z]{4}$/.test(code)) {
		say("The code is 8 letters and digits, like 7K3M-9QXA.");
		return;
	}
	$("pair").disabled = true;
	say("");
	toPlugin({ type: "pair", host: offered.host, port: offered.port, fingerprint: offered.fingerprint, code, name: $("device-name").value });
};
$("code").onkeydown = (e) => {
	if (e.key === "Enter") $("pair").click();
};
$("retry").onclick = () => toPlugin({ type: "retry" });
$("forget").onclick = () => toPlugin({ type: "forget" });
