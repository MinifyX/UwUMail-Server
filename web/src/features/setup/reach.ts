import type { GatewayView, Reachability } from "@/lib/api";
import type { CheckLineData, LineLevel } from "./lines";

const line = (code: string, level: LineLevel, params: Record<string, string | number> = {}): CheckLineData => ({
  code,
  level,
  params,
});

const capitalized = (value: string) => value.charAt(0).toUpperCase() + value.slice(1);

/** What the reachability check found: the public addresses, their reputation and network, and port 25. */
export function reachabilityLines(reach: Reachability): CheckLineData[] {
  const lines: CheckLineData[] = [];
  if (reach.addresses.length === 0) lines.push(line("publicAddressUnknown", "unknown"));
  const providers = new Set<string>();
  for (const address of reach.addresses) {
    const asn = address.asn ? `AS${address.asn}` : "";
    const network = [address.network, asn].filter(Boolean).join(", ");
    lines.push(line(network ? "publicAddress" : "publicAddressNoNetwork", "ok", { ip: address.ip, network }));
    if (address.homeConnection) lines.push(line("homeConnection", "problem", { ip: address.ip }));
    if (address.listed) lines.push(line("spamhausListed", "problem", { ip: address.ip }));
    if (address.spamhausUnknown) lines.push(line("spamhausUnknown", "unknown", { ip: address.ip }));
    else if (!address.homeConnection && !address.listed) lines.push(line("spamhausClean", "ok", { ip: address.ip }));
    if (address.genericPtr && !address.homeConnection) {
      lines.push(line("ptrGeneric", "warning", { ip: address.ip, name: address.ptr[0] ?? "–" }));
    }
    if (address.provider && !providers.has(address.provider.key)) {
      providers.add(address.provider.key);
      lines.push(line(`provider${capitalized(address.provider.key)}`, "warning", { network }));
    }
  }

  const { outbound } = reach;
  if (outbound.ok) {
    lines.push(line(reach.throughGateway ? "gatewayOk" : "directOk", "ok", { target: outbound.target }));
  } else if (outbound.stage === "dns") {
    lines.push(line("directDns", "warning", { error: outbound.error ?? "" }));
  } else {
    lines.push(
      line(reach.throughGateway ? "gatewayPort25Blocked" : "port25Blocked", "problem", { target: outbound.target }),
    );
  }
  if (reach.inbound) {
    const { inbound } = reach;
    lines.push(
      inbound.reachable && inbound.ours
        ? line("inboundOk", "ok", { ip: inbound.ip })
        : line("inboundSelfCall", "unknown", { ip: inbound.ip }),
    );
  }
  return lines;
}

/** The recommendation as a key under `setup.reach.recommend`. */
export function recommendation(reach: Reachability): "direct" | "gateway" | "unknown" | "paired" {
  return reach.throughGateway ? "paired" : reach.recommendation;
}

/** How the tunnel to the gateway is doing, and where the host name has to point. */
export function gatewayLines(view: GatewayView, hostname: string): CheckLineData[] {
  const addresses = view.addresses.join(", ");
  switch (view.state) {
    case "none":
      return [line("gatewayNone", "unknown")];
    case "connecting":
      return [line("gatewayConnecting", "warning", { tunnel: view.tunnel.join(", "), error: view.error ?? "" })];
    case "refused":
      return [line(`gatewayRefused${capitalized(view.refusal ?? "notPaired")}`, "problem")];
    case "connected":
      return [line("gatewayConnected", "ok", { addresses }), line("gatewayDns", "unknown", { hostname, addresses })];
  }
}
