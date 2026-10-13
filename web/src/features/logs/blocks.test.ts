import { describe, expect, it } from "vitest";
import { LOG_CATEGORIES, looksLikeNetwork, toggleCategory } from "./blocks";

describe("toggleCategory", () => {
  it("switches kinds on and off and keeps them in the order of the chips", () => {
    expect(toggleCategory([], "mail_in")).toEqual(["mail_in"]);
    expect(toggleCategory(["mail_in"], "login")).toEqual(["login", "mail_in"]);
    expect(toggleCategory(["login", "mail_in"], "login")).toEqual(["mail_in"]);
    expect(toggleCategory(["mail_in"], "mail_in")).toEqual([]);
    expect(LOG_CATEGORIES.reduce(toggleCategory, [])).toEqual(LOG_CATEGORIES);
  });
});

describe("looksLikeNetwork", () => {
  it("takes addresses and networks, v4 and v6", () => {
    expect(looksLikeNetwork("192.0.2.10")).toBe(true);
    expect(looksLikeNetwork(" 198.51.100.0/24 ")).toBe(true);
    expect(looksLikeNetwork("203.0.113.7/32")).toBe(true);
    expect(looksLikeNetwork("2001:db8::1")).toBe(true);
    expect(looksLikeNetwork("2001:db8::/32")).toBe(true);
    expect(looksLikeNetwork("2001:db8:0:0:0:0:0:1")).toBe(true);
    expect(looksLikeNetwork("::1")).toBe(true);
  });

  it("turns away what the server would not take either", () => {
    expect(looksLikeNetwork("")).toBe(false);
    expect(looksLikeNetwork("mail.example.com")).toBe(false);
    expect(looksLikeNetwork("192.0.2")).toBe(false);
    expect(looksLikeNetwork("192.0.2.256")).toBe(false);
    expect(looksLikeNetwork("192.0.2.0/33")).toBe(false);
    expect(looksLikeNetwork("192.0.2.0/24/8")).toBe(false);
    expect(looksLikeNetwork("192.0.2.0/")).toBe(false);
    expect(looksLikeNetwork("2001:db8::/129")).toBe(false);
    expect(looksLikeNetwork("2001:db8::1::2")).toBe(false);
    expect(looksLikeNetwork("2001:db8")).toBe(false);
    expect(looksLikeNetwork("2001:db8:0:0:0:0:0:0:1")).toBe(false);
    expect(looksLikeNetwork("2001:db8::12345")).toBe(false);
  });
});
