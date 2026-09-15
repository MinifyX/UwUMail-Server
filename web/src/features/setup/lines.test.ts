import { describe, expect, it } from "vitest";
import type { AddressReport, ServerCheck } from "@/lib/api";
import { blocklistLines, ptrLines, receivingLines, sendingLines, worstLevel } from "./lines";

const address = (ip: string, extra: Partial<AddressReport> = {}): AddressReport => ({
  ip,
  private: false,
  ptr: ["mail.example.com"],
  ptrConfirmed: true,
  ptrIsHostname: true,
  listings: [],
  ...extra,
});

const check = (extra: Partial<ServerCheck> = {}): ServerCheck => ({
  checkedAt: 0,
  hostname: "mail.example.com",
  addresses: [address("192.0.2.10")],
  route: "direct",
  relayHost: null,
  relayAddresses: [],
  outbound: { at: 0, route: "direct", target: "mx.example.net:25", ok: true, stage: null, error: null },
  inbound: [{ ip: "192.0.2.10", reachable: true, ours: true, greeting: "220 mail.example.com", error: null }],
  upstream: false,
  blocklistsChecked: false,
  ...extra,
});

const codes = (lines: { code: string }[]) => lines.map((entry) => entry.code);

describe("server check lines", () => {
  it("a healthy direct server is fine everywhere", () => {
    const report = check();
    expect(codes(sendingLines(report))).toEqual(["directOk"]);
    expect(codes(receivingLines(report))).toEqual(["inboundOk"]);
    expect(codes(ptrLines(report))).toEqual(["ptrOk"]);
    expect(worstLevel([...sendingLines(report), ...receivingLines(report)])).toBe("ok");
  });

  it("a blocked port 25 offers the relay", () => {
    const report = check({
      outbound: {
        at: 0,
        route: "direct",
        target: "mx.example.net:25",
        ok: false,
        stage: "connect",
        error: "timed out",
      },
    });
    expect(sendingLines(report)).toEqual([
      { code: "port25Blocked", level: "problem", params: { target: "mx.example.net:25" }, relay: true },
    ]);
  });

  it("a relay that refuses the login points at the credentials", () => {
    const report = check({
      route: "relay",
      relayHost: "relay.example.net",
      outbound: { at: 0, route: "relay", target: "relay.example.net:587", ok: false, stage: "login", error: "535" },
    });
    expect(codes(sendingLines(report))).toEqual(["relayLogin"]);
    expect(codes(ptrLines(report))).toEqual(["ptrRelay"]);
  });

  it("private addresses and missing reverse names are explained", () => {
    const report = check({ addresses: [address("192.168.1.20", { private: true })], inbound: [] });
    expect(codes(receivingLines(report))).toEqual(["privateOnly"]);
    expect(codes(ptrLines(report))).toEqual(["ptrPrivate"]);
    const noPtr = check({ addresses: [address("192.0.2.10", { ptr: [], ptrConfirmed: false, ptrIsHostname: false })] });
    expect(ptrLines(noPtr)[0]).toMatchObject({ code: "ptrMissing", level: "problem" });
  });

  it("blocklists show listings, unknown answers and clean addresses", () => {
    const report = check({
      blocklistsChecked: true,
      addresses: [
        address("192.0.2.10", {
          listings: [
            { list: "Spamhaus ZEN", status: "listed", answer: "127.0.0.2" },
            { list: "SpamCop", status: "unknown", answer: null },
            { list: "Barracuda", status: "clean", answer: null },
          ],
        }),
        address("192.0.2.11", { listings: [{ list: "SpamCop", status: "clean", answer: null }] }),
      ],
    });
    expect(codes(blocklistLines(report))).toEqual(["listed", "listUnknown", "clean", "clean"]);
    expect(blocklistLines(report)[2]?.params).toEqual({ ip: "192.0.2.10", lists: "Barracuda" });
    expect(blocklistLines(check())).toEqual([]);
  });
});
