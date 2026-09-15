import { describe, expect, it } from "vitest";
import { matchPath } from "./router";

describe("matchPath", () => {
  it("reads parameters and decodes them", () => {
    expect(matchPath("/admin/people/:login", "/admin/people/leni%40verein.de")).toEqual({ login: "leni@verein.de" });
    expect(matchPath("/password/:token", "/password/abc123")).toEqual({ token: "abc123" });
  });

  it("needs every segment to match", () => {
    expect(matchPath("/admin/people/:login", "/admin/people")).toBeNull();
    expect(matchPath("/admin/people", "/admin/people/")).toEqual({});
    expect(matchPath("/admin/people", "/admin/log")).toBeNull();
  });
});
