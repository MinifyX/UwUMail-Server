import { describe, expect, it } from "vitest";
import type { BimiCertificate, BimiView } from "@/lib/api";
import { certificateWarnings, dmarcReasons, isHexColor, logoPreviewPath } from "./bimi";

const dmarc = (extra: Partial<BimiView["dmarc"]>): BimiView["dmarc"] => ({
  status: "weak",
  policy: "quarantine",
  pct: null,
  subdomainPolicy: null,
  record: "v=DMARC1; p=quarantine",
  ...extra,
});

const certificate = (extra: Partial<BimiCertificate>): BimiCertificate => ({
  kind: "cmc",
  subject: "CN=Example Org",
  issuer: "CN=Example CA",
  notBefore: 100,
  notAfter: 1000,
  expired: false,
  names: ["example.com"],
  coversDomain: true,
  hasLogotype: true,
  ...extra,
});

describe("dmarcReasons", () => {
  it("has nothing to say when DMARC is fine", () => {
    expect(dmarcReasons(dmarc({ status: "ok" }))).toEqual([]);
  });

  it("says when it was not checked or is missing", () => {
    expect(dmarcReasons(dmarc({ status: "unknown" }))).toEqual(["notChecked"]);
    expect(dmarcReasons(dmarc({ status: "missing", policy: null, record: null }))).toEqual(["missing"]);
  });

  it("names each weakness", () => {
    expect(dmarcReasons(dmarc({ policy: "none" }))).toEqual(["policy"]);
    expect(dmarcReasons(dmarc({ policy: "reject", pct: 50 }))).toEqual(["pct"]);
    expect(dmarcReasons(dmarc({ policy: "Quarantine", subdomainPolicy: "none" }))).toEqual(["subdomains"]);
    expect(dmarcReasons(dmarc({ policy: "none", pct: 10, subdomainPolicy: "none" }))).toEqual([
      "policy",
      "pct",
      "subdomains",
    ]);
  });

  it("falls back to a general sentence", () => {
    expect(dmarcReasons(dmarc({ policy: "reject", pct: 100 }))).toEqual(["weak"]);
  });
});

describe("certificateWarnings", () => {
  it("accepts a good certificate", () => {
    expect(certificateWarnings(certificate({}), 500)).toEqual([]);
  });

  it("warns about validity, names, logo and kind", () => {
    expect(certificateWarnings(certificate({ expired: true }), 500)).toEqual(["expired"]);
    expect(certificateWarnings(certificate({}), 2000)).toEqual(["expired"]);
    expect(certificateWarnings(certificate({}), 50)).toEqual(["notYetValid"]);
    expect(
      certificateWarnings(certificate({ coversDomain: false, hasLogotype: false, kind: "unknown" }), 500),
    ).toEqual(["otherDomain", "noLogo", "unknownKind"]);
  });
});

describe("helpers", () => {
  it("makes a new preview address for each logo", () => {
    expect(logoPreviewPath("example.com", 42)).toBe("/api/admin/domains/example.com/bimi/logo.svg?v=42");
    expect(logoPreviewPath("ex ample.test", null)).toBe("/api/admin/domains/ex%20ample.test/bimi/logo.svg?v=0");
  });

  it("checks colours", () => {
    expect(isHexColor("#ffffff")).toBe(true);
    expect(isHexColor("#FF00aa")).toBe(true);
    expect(isHexColor("#fff")).toBe(false);
    expect(isHexColor("white")).toBe(false);
  });
});
