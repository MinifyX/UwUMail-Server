import type { AddressReport, ServerCheck } from "@/lib/api";

export type LineLevel = "ok" | "unknown" | "warning" | "problem";

/** One finding of the server check; the text comes from `setup.lines.<code>`, a hint from `setup.hints.<code>`. */
export interface CheckLineData {
  code: string;
  level: LineLevel;
  params: Record<string, string | number>;
  /** Offers the relay form next to it. */
  relay?: boolean;
}

const line = (
  code: string,
  level: LineLevel,
  params: Record<string, string | number> = {},
  extra: Partial<CheckLineData> = {},
): CheckLineData => ({ code, level, params, ...extra });

/** Whether mail gets out: straight to other servers on port 25, or through the relay. */
export function sendingLines(check: ServerCheck): CheckLineData[] {
  const { outbound } = check;
  if (check.route === "relay") {
    const host = check.relayHost ?? outbound.target;
    if (outbound.ok) return [line("relayOk", "ok", { host })];
    const code = outbound.stage === "login" ? "relayLogin" : outbound.stage === "tls" ? "relayTls" : "relayUnreachable";
    return [line(code, "problem", { host }, { relay: true })];
  }
  if (outbound.ok) return [line("directOk", "ok", { target: outbound.target })];
  if (outbound.stage === "dns") return [line("directDns", "warning", { error: outbound.error ?? "" })];
  return [line("port25Blocked", "problem", { target: outbound.target }, { relay: true })];
}

/** Whether other servers can reach port 25, as far as it can be seen from the server itself. */
export function receivingLines(check: ServerCheck): CheckLineData[] {
  if (check.upstream) return [line("upstream", "ok")];
  if (check.addresses.length === 0) return [line("noAddress", "problem", { hostname: check.hostname })];
  const lines: CheckLineData[] = [];
  if (check.addresses.every((address) => address.private)) {
    const ips = check.addresses.map((address) => address.ip).join(", ");
    lines.push(line("privateOnly", "warning", { hostname: check.hostname, ips }));
  }
  for (const inbound of check.inbound) {
    if (inbound.reachable && inbound.ours) lines.push(line("inboundOk", "ok", { ip: inbound.ip }));
    else if (inbound.reachable) {
      lines.push(line("inboundOther", "warning", { ip: inbound.ip, greeting: inbound.greeting ?? "" }));
    } else lines.push(line("inboundClosed", "warning", { ip: inbound.ip }));
  }
  return lines;
}

function ptrLine(address: AddressReport, hostname: string): CheckLineData {
  const name = address.ptr[0] ?? "";
  const params = { ip: address.ip, name, hostname };
  if (address.ptr.length === 0) return line("ptrMissing", "problem", params);
  if (!address.ptrConfirmed) return line("ptrUnconfirmed", "warning", params);
  if (!address.ptrIsHostname) return line("ptrOtherName", "warning", params);
  return line("ptrOk", "ok", params);
}

/** The reverse names of the addresses other servers see as the sender. */
export function ptrLines(check: ServerCheck): CheckLineData[] {
  if (check.route === "relay") return [line("ptrRelay", "ok", { host: check.relayHost ?? "" })];
  const public_ = check.addresses.filter((address) => !address.private);
  if (public_.length === 0) return [line("ptrPrivate", "unknown")];
  return public_.map((address) => ptrLine(address, check.hostname));
}

/** Blocklist results of the sending addresses: every listing, every silent list, and the clean ones together. */
export function blocklistLines(check: ServerCheck): CheckLineData[] {
  if (!check.blocklistsChecked) return [];
  const addresses = (check.route === "relay" ? check.relayAddresses : check.addresses).filter(
    (address) => address.listings.length > 0,
  );
  if (addresses.length === 0) return [line("noPublicAddress", "unknown")];
  return addresses.flatMap((address) => {
    const listed = address.listings.filter((listing) => listing.status === "listed");
    const unknown = address.listings.filter((listing) => listing.status === "unknown");
    const lines = [
      ...listed.map((listing) =>
        line("listed", "problem", { ip: address.ip, list: listing.list, answer: listing.answer ?? "" }),
      ),
      ...unknown.map((listing) => line("listUnknown", "unknown", { ip: address.ip, list: listing.list })),
    ];
    const clean = address.listings.filter((listing) => listing.status === "clean").map((listing) => listing.list);
    if (clean.length > 0) lines.push(line("clean", "ok", { ip: address.ip, lists: clean.join(", ") }));
    return lines;
  });
}

const ORDER: LineLevel[] = ["ok", "unknown", "warning", "problem"];

export function worstLevel(lines: CheckLineData[]): LineLevel {
  return lines.reduce<LineLevel>(
    (worst, entry) => (ORDER.indexOf(entry.level) > ORDER.indexOf(worst) ? entry.level : worst),
    "ok",
  );
}
