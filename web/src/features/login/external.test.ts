import { describe, expect, it } from "vitest";
import { loginNext, oidcError, oidcStartUrl, pendingLogin, withoutHandover } from "./external";

describe("oidcError", () => {
  it("knows the codes the server sends", () => {
    expect(oidcError("?oidcError=expired")).toBe("expired");
    expect(oidcError("?oidcError=domainNotAllowed")).toBe("domainNotAllowed");
  });

  it("treats anything else as a failed login, and nothing as no error", () => {
    expect(oidcError("?oidcError=somethingNew")).toBe("failed");
    expect(oidcError("?next=%2Fmail")).toBeNull();
    expect(oidcError("")).toBeNull();
  });
});

describe("pendingLogin", () => {
  it("turns the methods into the challenge the second step expects", () => {
    expect(pendingLogin("?pending=abc&methods=totp,recovery&next=%2Fmail")).toEqual({
      token: "abc",
      totp: true,
      passkey: false,
      recoveryCodes: true,
    });
    expect(pendingLogin("?pending=abc&methods=passkey")).toEqual({
      token: "abc",
      totp: false,
      passkey: true,
      recoveryCodes: false,
    });
  });

  it("needs a token", () => {
    expect(pendingLogin("?methods=totp")).toBeNull();
    expect(pendingLogin("?pending=")).toBeNull();
  });
});

describe("withoutHandover", () => {
  it("keeps only where to go next", () => {
    expect(withoutHandover("/login", "?pending=abc&methods=totp&next=%2Faccount")).toBe("/login?next=%2Faccount");
    expect(withoutHandover("/login", "?oidcError=refused")).toBe("/login");
  });
});

describe("loginNext", () => {
  it("reads next on the login page", () => {
    expect(loginNext("/login", "?next=%2Faccount%2Fsecurity")).toBe("/account/security");
    expect(loginNext("/login", "?next=https%3A%2F%2Fevil.example")).toBeNull();
    expect(loginNext("/", "")).toBeNull();
  });

  it("comes back to the page that asked for the login", () => {
    expect(loginNext("/oauth/authorize", "?client_id=uwu-1&state=x")).toBe("/oauth/authorize?client_id=uwu-1&state=x");
  });
});

describe("oidcStartUrl", () => {
  it("passes next along encoded", () => {
    expect(oidcStartUrl(null)).toBe("/api/auth/oidc/start");
    expect(oidcStartUrl("/oauth/authorize?client_id=a&state=b")).toBe(
      "/api/auth/oidc/start?next=%2Foauth%2Fauthorize%3Fclient_id%3Da%26state%3Db",
    );
  });
});
