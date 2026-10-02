import { describe, expect, it } from "vitest";
import {
  ALL_DOMAINS,
  domainChange,
  editorStart,
  fillPlaceholders,
  replacedByAllDomains,
  tooLarge,
  type SignatureOverview,
} from "./signatures";

const overview = (patch: Partial<SignatureOverview> = {}): SignatureOverview => ({
  state: "1",
  allDomains: null,
  domains: [
    { domain: "example.net", addressCount: 1, signature: null, company: null, source: "none" },
    {
      domain: "example.org",
      addressCount: 3,
      signature: null,
      company: { mode: "template", text: "Firma {name}", html: "" },
      source: "company",
    },
  ],
  identities: [],
  ...patch,
});

describe("placeholders", () => {
  it("are filled per address and escaped in HTML", () => {
    expect(fillPlaceholders("{name} <{adresse}> {domain} {address} {unknown}", "Mini", "mini@example.org", false)).toBe(
      "Mini <mini@example.org> example.org mini@example.org {unknown}",
    );
    expect(fillPlaceholders("<b>{name}</b>", '<img src=x onerror="alert(1)">', "a@example.org", true)).toBe(
      "<b>&lt;img src=x onerror=&quot;alert(1)&quot;&gt;</b>",
    );
    expect(fillPlaceholders("{{name}} {", "Ä", "a@example.org", false)).toBe("{Ä} {");
  });
});

describe("the domain editor", () => {
  it("starts with the domain's own, else every domain's, else the template", () => {
    expect(editorStart(overview(), "example.org")).toMatchObject({ origin: "template", targets: ["example.org"] });
    expect(editorStart(overview(), "example.net")).toMatchObject({ origin: "empty" });
    const all = overview({ allDomains: { text: "Alle", html: "" } });
    expect(editorStart(all, "example.net")).toMatchObject({ origin: "allDomains", targets: [ALL_DOMAINS] });
    const own = overview({
      domains: [
        {
          domain: "example.net",
          addressCount: 1,
          signature: { text: "Net", html: "" },
          company: null,
          source: "domain",
        },
      ],
    });
    expect(editorStart(own, "example.net").signature.text).toBe("Net");
  });

  it("applies one signature to several domains, or to all of them", () => {
    const signature = { text: "Hi", html: "" };
    expect(domainChange(overview(), ["example.org", "example.net"], signature)).toEqual({
      domains: { "example.org": signature, "example.net": signature },
    });
    const withOwn = overview({
      domains: [
        {
          domain: "example.net",
          addressCount: 1,
          signature: { text: "Net", html: "" },
          company: null,
          source: "domain",
        },
        { domain: "example.org", addressCount: 1, signature: null, company: null, source: "none" },
      ],
    });
    expect(domainChange(withOwn, [ALL_DOMAINS], signature)).toEqual({
      domains: { [ALL_DOMAINS]: signature, "example.net": null },
    });
    expect(replacedByAllDomains(withOwn, "example.org")).toEqual(["example.net"]);
    expect(domainChange(withOwn, ["example.org"], null)).toEqual({ domains: { "example.org": null } });
  });

  it("knows the size limit in UTF-8 bytes", () => {
    expect(tooLarge({ text: "ü".repeat(10), html: "" }, 19)).toBe(true);
    expect(tooLarge({ text: "ü".repeat(10), html: "" }, 20)).toBe(false);
  });
});
