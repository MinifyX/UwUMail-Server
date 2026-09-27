import { describe, expect, it } from "vitest";
import { afterLogin, isPortalPath, safeNext } from "./afterLogin";

describe("safeNext", () => {
  it("keeps paths on this server", () => {
    expect(safeNext("/mail")).toBe("/mail");
    expect(safeNext("/account/security")).toBe("/account/security");
    expect(safeNext("/mail/?x=1")).toBe("/mail/?x=1");
  });

  it("keeps the consent page of an app with its whole request", () => {
    const consent = `/oauth/authorize?response_type=code&client_id=uwu-${"x".repeat(32)}&state=${"s".repeat(300)}`;
    expect(safeNext(consent)).toBe(consent);
  });

  it("refuses anything that could lead somewhere else", () => {
    expect(safeNext("//evil.example")).toBeNull();
    expect(safeNext("https://evil.example")).toBeNull();
    expect(safeNext("/\\evil.example")).toBeNull();
    expect(safeNext("mail")).toBeNull();
    expect(safeNext("/mail\nSet-Cookie: x=1")).toBeNull();
    expect(safeNext(`/${"a".repeat(3000)}`)).toBeNull();
    expect(safeNext(null)).toBeNull();
    expect(safeNext("")).toBeNull();
  });
});

describe("afterLogin", () => {
  it("goes to the mailbox when there is one", () => {
    expect(afterLogin("", true)).toBe("/mail");
    expect(afterLogin("", false)).toBe("/account");
  });

  it("follows a safe next over the mailbox", () => {
    expect(afterLogin("?next=%2Faccount%2Fsecurity", true)).toBe("/account/security");
    expect(afterLogin("?next=https%3A%2F%2Fevil.example", true)).toBe("/mail");
  });
});

describe("isPortalPath", () => {
  it("knows the webmail is a page of its own", () => {
    expect(isPortalPath("/account")).toBe(true);
    expect(isPortalPath("/mail")).toBe(false);
    expect(isPortalPath("/mail/inbox")).toBe(false);
  });
});
