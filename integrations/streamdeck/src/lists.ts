/**
 * What the property inspector's lists offer, and the names keys show, read from
 * JStream's live state. Pure functions, so they're tested without Stream Deck.
 */
import type { JsonObject } from "@elgato/utils";

import type { HotkeyBinding, StateSnapshot } from "./jstream/protocol.ts";
import { effectKey, listEffects } from "./jstream/studio.ts";

/** One option in a property inspector list. */
export type Option = { value: string; label: string; group?: string };

/** Everything the property inspector's lists offer, from the live state. */
export function lists(state: StateSnapshot | undefined): JsonObject {
	if (!state) {
		return {};
	}
	const sceneName = new Map(state.scenes.map((s) => [s.scene_id, s.name]));
	const sourceName = new Map(state.sources.map((s) => [s.source_id, s.name]));
	const scenes: Option[] = state.scenes.filter((s) => !s.variant_of).map((s) => ({ value: s.scene_id, label: s.name }));
	const keyframes: Option[] = state.scenes
		.filter((s) => s.variant_of)
		.map((s) => ({ value: s.scene_id, label: s.name, group: sceneName.get(s.variant_of!) ?? s.variant_of! }));
	const items: Record<string, Option[]> = {};
	for (const scene of state.scenes.filter((s) => !s.variant_of)) {
		items[scene.scene_id] = (scene.items ?? []).map((i) => ({
			value: i.item_id,
			label: (i.source_id && sourceName.get(i.source_id)) || i.item_id,
		}));
	}
	const sources: Option[] = state.sources.map((s) => ({
		value: s.source_id,
		label: s.name,
		group: s.audio && s.audio.enabled !== false ? "With audio" : "Other sources",
	}));
	const sequences: Option[] = state.scenes.flatMap((s) =>
		(s.timelines ?? []).map((t) => ({ value: t.timeline_id, label: t.name || t.timeline_id, group: s.name })),
	);
	const effects: Option[] = listEffects(state).map((e) => ({ value: `${effectKey(e.owner)}|${e.filter_id}`, label: e.label }));
	const outputs: Option[] = (state.outputs ?? []).map((o) => ({ value: o.output_id, label: outputLabel(o) }));
	const channels: Option[] = (state.channels ?? []).map((c) => ({ value: c.channel_id, label: c.name || c.channel_id }));
	const hotkeys: Option[] = (state.hotkeys ?? []).map((h) => ({
		value: h.chord,
		label: `${h.chord}: ${describeAction(h, sceneName, sourceName)}`,
	}));
	const shows: Option[] = (state.shows ?? []).map((s) => ({ value: s.show_id, label: s.name || s.show_id }));
	const bookmarks: Record<string, Option[]> = {};
	for (const show of state.shows ?? []) {
		bookmarks[show.show_id] = (show.bookmarks ?? []).map((b) => ({ value: b.bookmark_id, label: b.name || b.bookmark_id }));
	}
	const transitions: Option[] = (state.transitions?.presets ?? []).map((t) => ({ value: t.preset_id, label: t.name || t.preset_id }));
	return { shows, bookmarks, scenes, keyframes, items, sources, sequences, effects, outputs, channels, hotkeys, transitions } as unknown as JsonObject;
}

/** A name for an output, which has none of its own: what it records to or streams to. */
export function outputLabel(output: { output_id: string; kind: { type: string; [key: string]: unknown } }): string {
	const kind = output.kind;
	if (kind.type === "record") {
		const path = typeof kind.path === "string" ? kind.path : "";
		const file = path.split(/[\\/]/).pop();
		return file ? `Record: ${file}` : `Record (${output.output_id})`;
	}
	if (kind.type === "rtmp") {
		const target = kind.target as { type?: string; url?: string } | undefined;
		if (target?.type === "youtube") {
			return `Stream: YouTube (${output.output_id})`;
		}
		try {
			return `Stream: ${new URL(target?.url ?? "").hostname} (${output.output_id})`;
		} catch {
			return `Stream (${output.output_id})`;
		}
	}
	if (kind.type === "virtual_cam") {
		return "Virtual camera";
	}
	return `${kind.type} (${output.output_id})`;
}

/** A hotkey's action in a few words, for the list. */
export function describeAction(binding: HotkeyBinding, sceneName: Map<string, string>, sourceName: Map<string, string>): string {
	const a = binding.action;
	const scene = typeof a.scene_id === "string" ? sceneName.get(a.scene_id) ?? a.scene_id : undefined;
	switch (a.type) {
		case "scene.switch":
			return `switch to ${scene}`;
		case "scene.set_keyframe":
			return `keyframe ${typeof a.keyframe_id === "string" ? sceneName.get(a.keyframe_id) ?? a.keyframe_id : "base"}`;
		case "source.restart":
			return `restart ${typeof a.source_id === "string" ? sourceName.get(a.source_id) ?? a.source_id : "source"}`;
		default: {
			const target = Object.entries(a).find(([k, v]) => k !== "type" && typeof v === "string");
			return target ? `${a.type} ${target[1]}` : a.type;
		}
	}
}

/** The three ways a two-state key can act. */
export type Mode = "toggle" | "on" | "off";

/** Whether a key in `mode` turns its target on, given whether it is on now. */
export function wantsOn(mode: Mode | undefined, isOn: boolean): boolean {
	return mode === "on" ? true : mode === "off" ? false : !isOn;
}

/** Breaks a long name onto two lines so it fits a key's title. */
export function fitTitle(name: string | undefined): string | undefined {
	if (!name) {
		return undefined;
	}
	if (name.length <= 10 || name.includes("\n")) {
		return name;
	}
	const words = name.split(" ");
	let first = "";
	while (words.length > 1 && (first + " " + words[0]).trim().length <= 10) {
		first = (first + " " + words.shift()).trim();
	}
	return first ? `${first}\n${words.join(" ")}` : name;
}
