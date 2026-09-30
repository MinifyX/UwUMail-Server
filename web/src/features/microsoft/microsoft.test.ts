import { describe, expect, it } from "vitest";
import type { MicrosoftAddress, MicrosoftDomainCheck, MicrosoftIssue } from "@/lib/api";
import {
  actionOf,
  bannerFacts,
  dmarcNote,
  issueDomain,
  issueIp,
  ptrStatus,
  worstKind,
  worstStatus,
} from "./microsoft";

const issue = (extra: Partial<MicrosoftIssue>): MicrosoftIssue => ({
  id: 1,
  scope: "ip",
  subject: "203.0.113.25",
  kind: "blocked",
  group: "blockList",
  code: "S3150",
  ip: "203.0.113.25",
  domain: "example.com",
  reply: "550 5.7.1 Unfortunately, messages from [203.0.113.25] weren't sent.",
  firstSeen: 1,
  lastSeen: 2,
  count: 3,
  resolvedAt: null,
  resolvedBy: null,
  ...extra,
});

const address = (extra: Partial<MicrosoftAddress>): MicrosoftAddress => ({
  ip: "203.0.113.25",
  private: false,
  ptr: ["mail.example.com"],
  ptrConfirmed: true,
  ptrIsHostname: true,
  ...extra,
});

const domain = (extra: Partial<MicrosoftDomainCheck>): MicrosoftDomainCheck => ({
  domain: "example.com",
  checkedAt: 1,
  spf: "ok",
  dkim: "ok",
  dmarc: "ok",
  dmarcPolicy: "reject",
  dmarcPct: null,
  aligned: "ok",
  ...extra,
});

describe("issues", () => {
  it("maps each group to what can be done", () => {
    expect(actionOf("blockList")).toBe("delist");
    expect(actionOf("banned")).toBe("delist");
    expect(actionOf("ipRefused")).toBe("delist");
    expect(actionOf("throttled")).toBe("wait");
    expect(actionOf("authentication")).toBe("authenticate");
    expect(actionOf("dmarc")).toBe("authenticate");
  });

  it("finds the worst open kind and ignores resolved ones", () => {
    expect(worstKind([])).toBeNull();
    expect(worstKind([issue({ kind: "throttled", group: "throttled" })])).toBe("throttled");
    expect(
      worstKind([issue({ kind: "throttled", group: "throttled" }), issue({ kind: "authentication", scope: "domain" })]),
    ).toBe("authentication");
    expect(worstKind([issue({ kind: "throttled" }), issue({ kind: "blocked", resolvedAt: 5 })])).toBe("throttled");
    expect(worstKind([issue({ resolvedAt: 5 })])).toBeNull();
  });

  it("takes the subject by scope and falls back to the last one seen", () => {
    expect(issueIp(issue({ subject: "198.51.100.7" }))).toBe("198.51.100.7");
    expect(issueIp(issue({ scope: "domain", subject: "example.org", ip: "192.0.2.1" }))).toBe("192.0.2.1");
    expect(issueIp(issue({ subject: "", ip: "192.0.2.9" }))).toBe("192.0.2.9");
    expect(issueDomain(issue({ scope: "domain", subject: "example.org" }))).toBe("example.org");
    expect(issueDomain(issue({ domain: "example.net" }))).toBe("example.net");
  });

  it("names each code and address once for the banner", () => {
    const facts = bannerFacts([
      issue({ id: 1 }),
      issue({ id: 2, lastSeen: 9 }),
      issue({ id: 3, kind: "throttled", group: "throttled", code: "4.7.650", ip: "192.0.2.4", subject: "192.0.2.4" }),
      issue({ id: 4, code: "5.7.708", resolvedAt: 3 }),
      issue({ id: 5, scope: "domain", subject: "example.org", ip: "", kind: "authentication", code: "5.7.515" }),
    ]);
    expect(facts).toEqual({
      codes: ["S3150", "4.7.650", "5.7.515"],
      ips: ["203.0.113.25", "192.0.2.4"],
      delist: true,
    });
    expect(bannerFacts([issue({ kind: "throttled", group: "throttled" })]).delist).toBe(false);
  });
});

describe("checklist", () => {
  it("judges reverse DNS", () => {
    expect(ptrStatus(address({}))).toBe("ok");
    expect(ptrStatus(address({ ptrConfirmed: false }))).toBe("warning");
    expect(ptrStatus(address({ ptr: [], ptrConfirmed: false }))).toBe("problem");
    expect(ptrStatus(address({ ip: "10.0.0.2", private: true, ptr: [] }))).toBe("unknown");
  });

  it("explains DMARC by policy and pct", () => {
    expect(dmarcNote(domain({ checkedAt: null }))).toBe("notChecked");
    expect(dmarcNote(domain({ dmarcPolicy: null }))).toBe("missing");
    expect(dmarcNote(domain({ dmarcPolicy: "none" }))).toBe("none");
    expect(dmarcNote(domain({ dmarcPolicy: "quarantine", dmarcPct: 50 }))).toBe("partial");
    expect(dmarcNote(domain({ dmarcPolicy: "quarantine", dmarcPct: 100 }))).toBe("strict");
    expect(dmarcNote(domain({}))).toBe("strict");
  });

  it("picks the worst status", () => {
    expect(worstStatus([])).toBe("ok");
    expect(worstStatus(["ok", "unknown"])).toBe("unknown");
    expect(worstStatus(["warning", "problem", "ok"])).toBe("problem");
  });
});
