/**
 * TLS to JStream's network listener, trusted by the certificate's fingerprint
 * (ADR-0104 decision 5). JStream's certificate is self-signed, so no certificate
 * authority vouches for it: the person compares the fingerprint the plugin was offered
 * with the one JStream shows, once, and the plugin pins it. From then on every
 * connection checks the fingerprint before a single byte of HTTP (and so the device
 * token) is written.
 */
import { request as httpsRequest } from "node:https";
import type { IncomingMessage, RequestOptions } from "node:http";
import { connect as tlsConnect, type TLSSocket } from "node:tls";
import type { Duplex } from "node:stream";

/** How long a TLS handshake may take before the attempt fails. */
const HANDSHAKE_TIMEOUT_MS = 8000;

/** A fingerprint in the form JStream and browsers show: 32 upper-case hex pairs and colons. */
export function normalizeFingerprint(text: string): string | undefined {
	const hex = text.replace(/[^0-9a-fA-F]/g, "").toUpperCase();
	if (hex.length !== 64) {
		return undefined;
	}
	return hex.match(/../g)!.join(":");
}

/** Connects with TLS, without checking the certificate, and resolves once the handshake is done. */
function handshake(host: string, port: number): Promise<TLSSocket> {
	return new Promise((resolve, reject) => {
		const socket = tlsConnect({
			host,
			port,
			// An IP address can't be sent as SNI.
			servername: isIpAddress(host) ? undefined : host,
			// The certificate is self-signed: trust comes from its fingerprint, checked below.
			rejectUnauthorized: false,
			ALPNProtocols: ["http/1.1"],
		});
		const timer = setTimeout(() => {
			socket.destroy();
			reject(new Error(`no TLS answer from ${host}:${port} within ${HANDSHAKE_TIMEOUT_MS / 1000} s`));
		}, HANDSHAKE_TIMEOUT_MS);
		socket.once("secureConnect", () => {
			clearTimeout(timer);
			resolve(socket);
		});
		socket.once("error", (err) => {
			clearTimeout(timer);
			reject(err);
		});
	});
}

/** The SHA-256 fingerprint of the certificate a connected socket was offered. */
export function peerFingerprint(socket: TLSSocket): string | undefined {
	const cert = socket.getPeerCertificate();
	return cert && cert.fingerprint256 ? normalizeFingerprint(cert.fingerprint256) : undefined;
}

/**
 * Reads the fingerprint of the certificate JStream offers at `host:port`, sending
 * nothing. The person compares it with the one JStream shows before pairing.
 */
export async function probeFingerprint(host: string, port: number): Promise<string> {
	const socket = await handshake(host, port);
	const fingerprint = peerFingerprint(socket);
	socket.destroy();
	if (!fingerprint) {
		throw new Error("the server offered no certificate");
	}
	return fingerprint;
}

/** Raised when the server's certificate isn't the pinned one. */
export class FingerprintMismatch extends Error {
	readonly offered: string | undefined;

	constructor(offered: string | undefined) {
		super(
			"JStream's certificate has changed since this Stream Deck paired. If JStream renewed it, pair again; otherwise something on the network is pretending to be JStream.",
		);
		this.name = "FingerprintMismatch";
		this.offered = offered;
	}
}

/**
 * A `createConnection` for `https.request` and `ws` that hands over the socket only
 * once its certificate matches `pinned`, so no request bytes go to any other server.
 */
export function pinnedConnection(pinned: string) {
	return (options: { host?: string | null; hostname?: string | null; port?: number | string | null }, done: (err: Error | null, socket?: Duplex) => void): undefined => {
		const host = String(options.hostname ?? options.host ?? "");
		const port = Number(options.port);
		handshake(host, port).then(
			(socket) => {
				const offered = peerFingerprint(socket);
				if (offered !== pinned) {
					socket.destroy();
					done(new FingerprintMismatch(offered));
					return;
				}
				done(null, socket);
			},
			(err: Error) => done(err),
		);
		return undefined;
	};
}

/** Whether `host` is an IPv4 or IPv6 address rather than a name. */
export function isIpAddress(host: string): boolean {
	return /^\d{1,3}(\.\d{1,3}){3}$/.test(host) || host.includes(":");
}

/** The host as it goes in a URL: IPv6 addresses in brackets. */
export function urlHost(host: string): string {
	return host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
}

/** The answer to a pairing request. */
export type PairResult = { deviceId: string; token: string };

/**
 * Trades a pairing code for a device token (ADR-0044): `POST /api/v1/pair` over a
 * connection pinned to the fingerprint the person confirmed.
 */
export function pair(host: string, port: number, fingerprint: string, code: string, name: string): Promise<PairResult> {
	const body = JSON.stringify({ code: code.trim().toUpperCase(), name });
	const options: RequestOptions = {
		host,
		port,
		method: "POST",
		path: "/api/v1/pair",
		headers: {
			"Content-Type": "application/json",
			"Content-Length": Buffer.byteLength(body),
			Host: `${urlHost(host)}:${port}`,
		},
		createConnection: pinnedConnection(fingerprint) as unknown as RequestOptions["createConnection"],
	};
	return new Promise((resolve, reject) => {
		const req = httpsRequest(options, (res: IncomingMessage) => {
			let text = "";
			res.setEncoding("utf8");
			res.on("data", (chunk: string) => {
				if (text.length < 16384) {
					text += chunk;
				}
			});
			res.on("end", () => {
				if (res.statusCode === 200) {
					try {
						const answer = JSON.parse(text) as { device_id?: string; token?: string };
						if (answer.device_id && answer.token) {
							resolve({ deviceId: answer.device_id, token: answer.token });
							return;
						}
					} catch {
						// Falls through to the error below.
					}
					reject(new Error("JStream's answer to the pairing request was not understood"));
					return;
				}
				reject(new Error(pairingRefusal(res.statusCode ?? 0, text)));
			});
		});
		req.on("error", reject);
		req.end(body);
	});
}

/** What a refused pairing request means, in words for the person. */
export function pairingRefusal(status: number, body: string): string {
	switch (status) {
		case 401:
			return "No pairing code is open in JStream. Start pairing in JStream's Settings › Devices, then type the code it shows.";
		case 403:
			return body.trim() || "That code is not the one JStream shows. Check it and try again.";
		case 400:
			return "JStream refused the request. Check the code and the device name.";
		default:
			return `JStream answered ${status}${body.trim() ? `: ${body.trim()}` : ""}`;
	}
}
