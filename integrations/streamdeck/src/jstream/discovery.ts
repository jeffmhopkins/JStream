/**
 * Finding JStream on the network: an mDNS / DNS-SD query for `_jstream._tcp.local`, the
 * service JStream announces while network access and announcing are both on. An answer
 * names the instance (the computer's name), the network listener's port, and the
 * address it came from. Finding JStream proves nothing about it: pairing still needs
 * the code and the fingerprint JStream shows.
 */
import makeMdns from "multicast-dns";

/** The DNS-SD service JStream's network listener is announced as. */
export const SERVICE = "_jstream._tcp.local";

/** One JStream found on the network. */
export type Found = {
	/** The instance's name: the computer's name, as JStream announces it. */
	name: string;
	/** The address to connect to. */
	host: string;
	/** The network listener's port. */
	port: number;
	/** The host's `.local` name, when the answer gave one. */
	hostname?: string;
};

type Answer = { name: string; type: string; data: unknown };

/**
 * Asks the network for JStream for `waitMs` and resolves with what answered, one entry
 * per instance. Never rejects: no network, or nothing found, is an empty list.
 */
export function discover(waitMs = 2500): Promise<Found[]> {
	return new Promise((resolve) => {
		let mdns: ReturnType<typeof makeMdns>;
		try {
			mdns = makeMdns({ reuseAddr: true });
		} catch {
			resolve([]);
			return;
		}
		const found = new Map<string, Found>();
		const collect = new Collector();
		mdns.on("response", (packet: { answers?: Answer[]; additionals?: Answer[] }, rinfo: { address: string }) => {
			for (const entry of collect.add([...(packet.answers ?? []), ...(packet.additionals ?? [])], rinfo.address)) {
				found.set(entry.name, entry);
			}
		});
		mdns.on("error", () => undefined);
		mdns.on("warning", () => undefined);
		const ask = () => {
			try {
				mdns.query({ questions: [{ name: SERVICE, type: "PTR" }] });
			} catch {
				// No interface to send on.
			}
		};
		ask();
		const again = setTimeout(ask, Math.min(1000, waitMs / 2));
		setTimeout(() => {
			clearTimeout(again);
			try {
				mdns.destroy();
			} catch {
				// Already closed.
			}
			resolve([...found.values()].sort((a, b) => a.name.localeCompare(b.name)));
		}, waitMs);
	});
}

/**
 * Pieces DNS-SD answers together across packets: a PTR names an instance, its SRV gives
 * the port and target, an A or AAAA record the target's address. An instance whose
 * address never arrives uses the address its answer came from.
 */
export class Collector {
	#instances = new Set<string>();
	#srv = new Map<string, { port: number; target: string; from: string }>();
	#addresses = new Map<string, string>();

	/** Adds one packet's records, and returns the instances it completes. */
	add(records: Answer[], from: string): Found[] {
		for (const r of records) {
			const name = r.name.toLowerCase();
			if (r.type === "PTR" && name === SERVICE && typeof r.data === "string") {
				this.#instances.add(r.data);
			} else if (r.type === "SRV" && name.endsWith(`.${SERVICE}`)) {
				const data = r.data as { port: number; target: string };
				this.#srv.set(r.name, { port: data.port, target: data.target, from });
				this.#instances.add(r.name);
			} else if (r.type === "A" && typeof r.data === "string") {
				this.#addresses.set(name, r.data);
			}
		}
		const out: Found[] = [];
		for (const instance of this.#instances) {
			const srv = this.#srv.get(instance);
			if (!srv || srv.port <= 0) {
				continue;
			}
			const host = this.#addresses.get(srv.target.toLowerCase()) ?? srv.from;
			out.push({ name: instanceLabel(instance), host, port: srv.port, hostname: srv.target || undefined });
		}
		return out;
	}
}

/** The readable part of `Studio PC._jstream._tcp.local`. */
export function instanceLabel(instance: string): string {
	return instance.endsWith(`.${SERVICE}`) ? instance.slice(0, -SERVICE.length - 1) : instance;
}
