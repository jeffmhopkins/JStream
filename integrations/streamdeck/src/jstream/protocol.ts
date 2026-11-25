/**
 * The part of JStream's WebSocket protocol this plugin speaks
 * (crates/jstream-protocol/README.md). Only the fields the plugin reads are typed; the
 * server may send more, and newer servers add fields freely (R-API-001).
 */

/**
 * The protocol version this plugin says in `hello`. A server serves any client whose
 * minor is no newer than its own, so this names the oldest minor that has everything
 * used here: `channel.values` and `channel.switch` arrived in 0.169.
 */
export const PROTOCOL = { major: 0, minor: 169 } as const;

/** The path of the WebSocket API on every listener. */
export const WS_PATH = "/api/v1/ws";

/** The pairing exchange's path (ADR-0044). */
export const PAIR_PATH = "/api/v1/pair";

/** The network listener's default port (ADR-0104, `--lan-port`). */
export const DEFAULT_PORT = 4456;

export type FilterInfo = { filter_id: string; kind: string; enabled: boolean };
export type FilterChains = { video?: FilterInfo[]; audio?: FilterInfo[] };

export type ItemInfo = {
	item_id: string;
	source_id?: string;
	visible?: boolean;
	effects?: FilterInfo[] | null;
};

export type TimelineInfo = { timeline_id: string; name?: string | null };

export type SceneInfo = {
	scene_id: string;
	name: string;
	role?: string | null;
	variant_of?: string | null;
	items?: ItemInfo[];
	filters?: FilterChains | null;
	timelines?: TimelineInfo[];
};

export type SourceAudio = { enabled?: boolean; muted?: boolean; gain_db?: number };

export type SourceInfo = {
	source_id: string;
	name: string;
	audio?: SourceAudio | null;
	filters?: FilterChains | null;
};

export type OutputKind =
	| { type: "record"; path?: string }
	| { type: "rtmp"; target?: { type: string; url?: string; broadcast_id?: string } }
	| { type: string; [key: string]: unknown };

export type OutputInfo = { output_id: string; kind: OutputKind };

export type ChannelInfo = { channel_id: string; name?: string | null };

export type HotkeyBinding = { chord: string; action: Action; labels?: string[] };

/** A show (ADR-0210): a run order with bookmarks. Since protocol 0.179 or later; absent on older servers. */
export type ShowInfo = { show_id: string; name?: string | null; bookmarks?: { bookmark_id: string; name?: string | null }[] };

/** The show running, as `studio.get` reports it. */
export type RunningShow = { show_id: string; state: "playing" | "paused" | "off_script" | string; at_ms?: number; rehearse?: boolean };

export type StateSnapshot = {
	rev: number;
	scenes: SceneInfo[];
	sources: SourceInfo[];
	outputs?: OutputInfo[];
	channels?: ChannelInfo[];
	hotkeys?: HotkeyBinding[];
	shows?: ShowInfo[];
	transitions?: { presets?: { preset_id: string; name?: string }[]; default?: string };
};

export type StudioSnapshot = {
	program_scene?: string | null;
	preview_scene?: string | null;
	timelines?: { timeline_id: string; scene_id?: string }[];
	waiting?: { timeline_id: string }[];
	keyframes?: { scene_id: string; keyframe_id: string }[];
	show?: RunningShow | null;
};

export type ChannelValue = { channel_id: string; value: number; on: boolean };

/** An output's state, as `output.status` and `output.state_changed` give it. */
export type OutputState = "stopped" | "starting" | "active" | "reconnecting" | "stopping" | "failed";

/** An action: a JSON object tagged by `type`. */
export type Action = { type: string; [key: string]: unknown };

/** An edit, sent in a batch. */
export type Edit = { type: string; [key: string]: unknown };

export type Call =
	| { kind: "query"; query: { type: string; [key: string]: unknown } }
	| { kind: "action"; action: Action }
	| { kind: "batch"; edits: Edit[] };

export type ServerEvent = { type: string; [key: string]: unknown };

export type ServerMessage =
	| { type: "welcome"; protocol: { major: number; minor: number }; client_id: string; rev: number }
	| { type: "response"; id: string; result: { kind: string; result?: unknown; rev?: number } }
	| { type: "error"; id?: string; error: { code: string; message?: string; [key: string]: unknown } }
	| { type: "event"; event: ServerEvent };

/** An error the server answered a request with. */
export class JStreamError extends Error {
	readonly code: string;

	constructor(code: string, message?: string) {
		super(message ?? code);
		this.code = code;
		this.name = "JStreamError";
	}
}
