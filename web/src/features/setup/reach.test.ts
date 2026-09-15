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
});
