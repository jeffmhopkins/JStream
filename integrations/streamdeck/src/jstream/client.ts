/**
 * One connection to JStream's WebSocket API, over TLS pinned to the paired
 * certificate, authenticated with the device token. It says `hello`, answers requests
 * by ID, passes events on, and reconnects with a growing delay when the connection
 * drops.
 */
import { EventEmitter } from "node:events";
import WebSocket from "ws";

import { type Action, type Call, type Edit, JStreamError, PROTOCOL, type ServerEvent, type ServerMessage, WS_PATH } from "./protocol.ts";
import { pinnedConnection, urlHost } from "./tls.ts";

/** Where JStream is and the credentials pairing gave. */
export type Endpoint = {
	host: string;
	port: number;
	fingerprint: string;
	token: string;
};

/** The connection's state, as the property inspector shows it. */
export type ConnectionStatus =
	| { state: "unpaired" }
	| { state: "connecting" }
	| { state: "connected"; server: string }
	| { state: "error"; message: string; retryInMs?: number };

/** How long a request may wait for its answer. */
const REQUEST_TIMEOUT_MS = 10000;
/** Reconnect delays grow from the first to the last and stay there. */
const RECONNECT_MS = [1000, 2000, 5000, 10000, 20000];

type Pending = { resolve: (value: unknown) => void; reject: (err: Error) => void; timer: NodeJS.Timeout };

export interface ClientEvents {
	status: [ConnectionStatus];
	welcome: [];
	event: [ServerEvent];
}

export class JStreamClient extends EventEmitter<ClientEvents> {
	readonly #clientName: string;
	#endpoint: Endpoint | undefined;
	#socket: WebSocket | undefined;
	#welcomed = false;
	#pending = new Map<string, Pending>();
	#nextId = 1;
	#attempt = 0;
	#reconnectTimer: NodeJS.Timeout | undefined;
	#status: ConnectionStatus = { state: "unpaired" };

	constructor(clientName: string) {
		super();
		this.#clientName = clientName;
	}

	get status(): ConnectionStatus {
		return this.#status;
	}

	get connected(): boolean {
		return this.#welcomed;
	}

	/** Connects to `endpoint`, or disconnects with `undefined`. A call with the same endpoint does nothing. */
	setEndpoint(endpoint: Endpoint | undefined): void {
		if (sameEndpoint(endpoint, this.#endpoint)) {
			return;
		}
		this.#endpoint = endpoint;
		this.#attempt = 0;
		this.#teardown();
		if (endpoint) {
			this.#open();
		} else {
			this.#setStatus({ state: "unpaired" });
		}
	}

	/** Closes the connection and stops reconnecting. */
	close(): void {
		this.setEndpoint(undefined);
	}

	/** Reconnects now, skipping any wait. */
	retry(): void {
		if (this.#endpoint && !this.#welcomed) {
			this.#attempt = 0;
			this.#teardown();
			this.#open();
		}
	}

	query<T = unknown>(query: { type: string; [key: string]: unknown }): Promise<T> {
		return this.#request({ kind: "query", query }) as Promise<T>;
	}

	action(action: Action): Promise<unknown> {
		return this.#request({ kind: "action", action });
	}

	batch(edits: Edit[]): Promise<unknown> {
		return this.#request({ kind: "batch", edits });
	}

	#request(call: Call): Promise<unknown> {
		const socket = this.#socket;
		if (!socket || !this.#welcomed) {
			return Promise.reject(new JStreamError("not_connected", "not connected to JStream"));
		}
		const id = `sd${this.#nextId++}`;
		return new Promise((resolve, reject) => {
			const timer = setTimeout(() => {
				this.#pending.delete(id);
				reject(new JStreamError("timeout", "JStream didn't answer in time"));
			}, REQUEST_TIMEOUT_MS);
			this.#pending.set(id, { resolve, reject, timer });
			socket.send(JSON.stringify({ type: "request", id, call }));
		});
	}

	#open(): void {
		const endpoint = this.#endpoint!;
		this.#setStatus({ state: "connecting" });
		this.#lastError = undefined;
		this.#refused = false;
		const url = `wss://${urlHost(endpoint.host)}:${endpoint.port}${WS_PATH}`;
		const socket = new WebSocket(url, {
			headers: { Authorization: `Bearer ${endpoint.token}` },
			createConnection: pinnedConnection(endpoint.fingerprint) as never,
			handshakeTimeout: REQUEST_TIMEOUT_MS,
			maxPayload: 64 * 1024 * 1024,
			perMessageDeflate: false,
		});
		this.#socket = socket;
		socket.on("open", () => {
			socket.send(JSON.stringify({ type: "hello", protocol: PROTOCOL, client_name: this.#clientName }));
		});
		socket.on("message", (data) => {
			if (socket === this.#socket) {
				this.#receive(data.toString());
			}
		});
		socket.on("unexpected-response", (req, res) => {
			if (socket === this.#socket) {
				// A token JStream doesn't know won't start working: stop retrying until paired again.
				this.#refused = res.statusCode === 401;
				this.#lastError =
					res.statusCode === 401
						? "JStream doesn't know this Stream Deck any more (it was removed, or unused for 30 days). Pair again."
						: res.statusCode === 403
							? "JStream refused the connection's host name. Use the address JStream shows."
							: `JStream answered ${res.statusCode}`;
			}
			res.resume();
			// Aborting with an error makes ws report it and close the socket.
			req.destroy(new Error(this.#lastError ?? `JStream answered ${res.statusCode}`));
		});
		socket.on("error", (err) => {
			// The first error of an attempt says why; what follows it is its consequence.
			if (socket === this.#socket && this.#lastError === undefined) {
				this.#lastError = err.message;
			}
		});
		socket.on("close", () => {
			if (socket === this.#socket) {
				this.#dropped();
			}
		});
	}

	#lastError: string | undefined;
	#refused = false;

	#receive(text: string): void {
		let message: ServerMessage;
		try {
			message = JSON.parse(text) as ServerMessage;
		} catch {
			return;
		}
		switch (message.type) {
			case "welcome":
				this.#welcomed = true;
				this.#attempt = 0;
				this.#lastError = undefined;
				this.#setStatus({ state: "connected", server: `${message.protocol.major}.${message.protocol.minor}` });
				this.emit("welcome");
				break;
			case "response": {
				const pending = this.#take(message.id);
				pending?.resolve(message.result.result ?? message.result);
				break;
			}
			case "error": {
				const pending = message.id ? this.#take(message.id) : undefined;
				const err = new JStreamError(message.error.code, message.error.message);
				if (pending) {
					pending.reject(err);
				} else if (!this.#welcomed) {
					// The handshake was refused (an unsupported version, or the person declined).
					this.#lastError = err.message;
					this.#socket?.close();
				}
				break;
			}
			case "event":
				this.emit("event", message.event);
				break;
		}
	}

	#take(id: string): Pending | undefined {
		const pending = this.#pending.get(id);
		if (pending) {
			clearTimeout(pending.timer);
			this.#pending.delete(id);
		}
		return pending;
	}

	#dropped(): void {
		const wasWelcomed = this.#welcomed;
		this.#teardown();
		if (!this.#endpoint) {
			return;
		}
		const message = this.#lastError ?? (wasWelcomed ? "the connection to JStream dropped" : "JStream can't be reached");
		if (this.#refused) {
			this.#setStatus({ state: "error", message });
			return;
		}
		const delay = RECONNECT_MS[Math.min(this.#attempt, RECONNECT_MS.length - 1)];
		this.#attempt++;
		this.#setStatus({ state: "error", message, retryInMs: delay });
		this.#reconnectTimer = setTimeout(() => {
			this.#reconnectTimer = undefined;
			this.#open();
		}, delay);
	}

	#teardown(): void {
		if (this.#reconnectTimer) {
			clearTimeout(this.#reconnectTimer);
			this.#reconnectTimer = undefined;
		}
		const socket = this.#socket;
		this.#socket = undefined;
		this.#welcomed = false;
		if (socket) {
			socket.removeAllListeners("message");
			socket.on("error", () => undefined);
			socket.terminate();
		}
		for (const [id, pending] of this.#pending) {
			clearTimeout(pending.timer);
			pending.reject(new JStreamError("not_connected", "the connection to JStream closed"));
			this.#pending.delete(id);
		}
	}

	#setStatus(status: ConnectionStatus): void {
		this.#status = status;
		this.emit("status", status);
	}
}

function sameEndpoint(a: Endpoint | undefined, b: Endpoint | undefined): boolean {
	if (!a || !b) {
		return a === b;
	}
	return a.host === b.host && a.port === b.port && a.fingerprint === b.fingerprint && a.token === b.token;
}
