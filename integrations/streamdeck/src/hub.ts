/**
 * The plugin's one connection to JStream, shared by every key: where JStream is and
 * the pairing (kept in the plugin's global settings), the client, the live studio
 * state, and the messages the property inspector sends about the connection.
 */
import streamDeck from "@elgato/streamdeck";
import type { JsonObject, JsonValue } from "@elgato/utils";
import { hostname } from "node:os";

import { JStreamClient, type ConnectionStatus } from "./jstream/client.ts";
import { discover } from "./jstream/discovery.ts";
import { DEFAULT_PORT } from "./jstream/protocol.ts";
import { Studio } from "./jstream/studio.ts";
import { lists } from "./lists.ts";
import { askToPair, normalizeFingerprint, pair, probeFingerprint } from "./jstream/tls.ts";

/** The plugin's global settings: where JStream is and what pairing gave. */
export type GlobalSettings = {
	host?: string;
	port?: number;
	/** The certificate fingerprint the person confirmed, pinned. */
	fingerprint?: string;
	/** The device token pairing gave. */
	token?: string;
	deviceId?: string;
	/** The name this Stream Deck paired under, as JStream's device list shows it. */
	deviceName?: string;
};

export const client = new JStreamClient(`Stream Deck (${hostname()})`);
export const studio = new Studio(client);

let settings: GlobalSettings = {};

/**
 * The ask to pair (ADR-0235) waiting for the person's answer in JStream, if any. Pairing
 * with a code meanwhile aborts it, which ends the ask in JStream too.
 */
let asking: AbortController | undefined;

/** Reads the saved pairing and connects. */
export async function start(): Promise<void> {
	settings = await streamDeck.settings.getGlobalSettings<GlobalSettings>();
	apply();
	client.on("status", () => pushConnection().catch(logFailure));
	studio.on("changed", () => pushLists().catch(logFailure));
}

function apply(): void {
	const { host, port, fingerprint, token } = settings;
	client.setEndpoint(host && port && fingerprint && token ? { host, port, fingerprint, token } : undefined);
}

async function save(next: GlobalSettings): Promise<void> {
	settings = next;
	await streamDeck.settings.setGlobalSettings(settings as JsonObject);
	apply();
}

/** The connection as the property inspector shows it. */
function connectionPayload(): JsonObject {
	const status: ConnectionStatus = client.status;
	return {
		type: "connection",
		status: status as unknown as JsonObject,
		host: settings.host ?? "",
		port: settings.port ?? DEFAULT_PORT,
		fingerprint: settings.fingerprint ?? "",
		paired: Boolean(settings.token),
		deviceName: settings.deviceName ?? `Stream Deck (${hostname()})`,
	};
}

async function pushConnection(): Promise<void> {
	if (streamDeck.ui.action) {
		await streamDeck.ui.sendToPropertyInspector(connectionPayload());
	}
}

async function pushLists(): Promise<void> {
	if (streamDeck.ui.action) {
		await streamDeck.ui.sendToPropertyInspector({ type: "lists", lists: lists(studio.state) });
	}
}

/** What the property inspector may ask about the connection. */
type InspectorRequest =
	| { type: "hello" }
	| { type: "discover" }
	| { type: "probe"; host: string; port: number }
	| { type: "pair"; host: string; port: number; fingerprint: string; code: string; name: string }
	| { type: "ask"; host: string; port: number; fingerprint: string; name: string }
	| { type: "forget" }
	| { type: "retry" };

/** Answers a property inspector's message about the connection. Returns false for any other message. */
export async function onInspectorMessage(payload: JsonValue): Promise<boolean> {
	const request = payload as InspectorRequest;
	const reply = (message: JsonObject) => streamDeck.ui.sendToPropertyInspector(message);
	switch (request?.type) {
		case "hello":
			await reply(connectionPayload());
			await reply({ type: "lists", lists: lists(studio.state) });
			return true;
		case "discover":
			await reply({ type: "discovered", found: (await discover()) as unknown as JsonObject[] });
			return true;
		case "probe":
			try {
				const host = String(request.host).trim();
				const port = Number(request.port) || DEFAULT_PORT;
				await reply({ type: "probed", host, port, fingerprint: await probeFingerprint(host, port) });
			} catch (err) {
				await reply({ type: "failed", step: "probe", message: reason(err) });
			}
			return true;
		case "pair":
		case "ask":
			if (request.type === "ask") {
				// One ask at a time: pressing Connect again keeps waiting on the first.
				if (asking || settings.token) {
					return true;
				}
				asking = new AbortController();
			} else {
				asking?.abort();
			}
			// "ask" pairs once the person presses Allow in JStream (ADR-0235), trusting the
			// certificate the probe saw; "pair" trades the code the person typed (ADR-0044).
			try {
				const fingerprint = normalizeFingerprint(String(request.fingerprint));
				if (!fingerprint) {
					throw new Error("the fingerprint isn't one JStream gave");
				}
				const host = String(request.host).trim();
				const port = Number(request.port) || DEFAULT_PORT;
				const name = String(request.name || `Stream Deck (${hostname()})`).trim().slice(0, 64);
				const result =
					request.type === "ask"
						? await askToPair(host, port, fingerprint, name, asking?.signal)
						: await pair(host, port, fingerprint, String(request.code), name);
				await save({ host, port, fingerprint, token: result.token, deviceId: result.deviceId, deviceName: name });
				await reply({ type: "paired" });
				await pushConnection();
			} catch (err) {
				await reply({ type: "failed", step: request.type, message: reason(err) });
			} finally {
				if (request.type === "ask") {
					asking = undefined;
				}
			}
			return true;
		case "forget":
			await save({});
			await pushConnection();
			return true;
		case "retry":
			client.retry();
			return true;
		default:
			return false;
	}
}

function logFailure(err: unknown): void {
	streamDeck.logger.error(`property inspector update failed: ${reason(err)}`);
}

function reason(err: unknown): string {
	if (err instanceof Error) {
		const code = (err as NodeJS.ErrnoException).code;
		switch (code) {
			case "ECONNREFUSED":
				return "Nothing answered on that port. Is JStream's network access on, and is the port right?";
			case "ENOTFOUND":
			case "EAI_AGAIN":
				return "That name can't be found on the network. Try the computer's IP address.";
			case "EHOSTUNREACH":
			case "ENETUNREACH":
			case "ETIMEDOUT":
				return "That address can't be reached from this computer.";
		}
		return err.message;
	}
	return String(err);
}
