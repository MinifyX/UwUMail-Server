import { describe, expect, it } from "vitest";
import { allowedDomains } from "./MaskedCards";

describe("the domains a masked address policy allows", () => {
  it("follows the mode, sorted by name, as the server works it out", () => {
    const masked = ["b.example", "a.example"];
    expect(allowedDomains("off", "example.org", masked)).toEqual([]);
    expect(allowedDomains("own", "example.org", masked)).toEqual(["example.org"]);
    expect(allowedDomains("dedicated", "example.org", masked)).toEqual(["a.example", "b.example"]);
    expect(allowedDomains("both", "example.org", masked)).toEqual(["a.example", "b.example", "example.org"]);
  });

  it("has no own domain for an account whose domain carries masked addresses only", () => {
    expect(allowedDomains("both", null, ["a.example"])).toEqual(["a.example"]);
    expect(allowedDomains("own", null, [])).toEqual([]);
  });
});
