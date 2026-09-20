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
      return [
        line("gatewayConnected", "ok", { addresses }),
        line("gatewayDns", "unknown", { hostname, addresses }),
        ...machineLines(view),
      ];
  }
}

/**
 * What the gateway says about the machine it runs on. Nobody logs into a VPS to see that it needs
 * updates, so it says so here instead. Missing for gateways from before they told us.
 */
function machineLines(view: GatewayView): CheckLineData[] {
  const machine = view.machine;
  if (!machine) return [];
  const lines: CheckLineData[] = [];

  const system = machine.system;
  if (system) {
    // The whole command, ready to paste: the gateway is a machine you reach over SSH and rarely
    // think about, so "there are updates" is only half an answer.
    const address = view.addresses[0];
    // The command is composed here from a constant and the reboot flag, never taken from the
    // gateway's report: a compromised gateway must not put a command in front of the admin to run
    // as root over SSH (security-audit-0.5.2 G-4).
    const command = system.rebootRequired
      ? "apt-get update && apt-get -y dist-upgrade && reboot"
      : "apt-get update && apt-get -y dist-upgrade";
    const ssh = address ? `ssh root@${address} '${command}'` : command;
    if (system.securityUpdates > 0) {
      lines.push(
        line("gatewaySecurityUpdates", "problem", {
          count: system.updates,
          security: system.securityUpdates,
          ssh,
        }),
      );
    } else if (system.updates > 0) {
      lines.push(line("gatewayUpdates", "warning", { count: system.updates, ssh }));
    } else {
      lines.push(line("gatewayUpToDate", "ok", { name: system.name }));
    }
    if (system.automaticSecurity) lines.push(line("gatewayAutomaticSecurity", "ok", {}));
    if (system.rebootRequired) lines.push(line("gatewayReboot", "warning", {}));
    if (system.newRelease) lines.push(line("gatewayNewRelease", "unknown", { release: system.newRelease }));
  }

  const protection = machine.protection;
  if (protection) {
    if (!protection.firewallActive) lines.push(line("gatewayNoFirewall", "problem", {}));
    else if (!protection.fail2ban) lines.push(line("gatewayNoFail2ban", "warning", { firewall: protection.firewall }));
    else {
      lines.push(
        line("gatewayProtected", "ok", {
          firewall: protection.firewall,
          banned: protection.banned,
          fromServer: protection.fromServer,
        }),
      );
    }
  }

  // The one that matters most when something goes wrong: no ban on the gateway may lock this
  // server out, and this is where you can see that it knows where the server is.
  if (machine.trusted.length > 0) lines.push(line("gatewayTrusted", "ok", { addresses: machine.trusted.join(", ") }));
  return lines;
}
