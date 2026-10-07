import { describe, expect, it } from "vitest";
import { appPasswordOnly, decisionBody, knownScopes, masksOnly } from "./authorize";

describe("decisionBody", () => {
  it("passes every parameter of the request on as it came", () => {
    const search =
      "?response_type=code&client_id=uwu-abc&redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2F&scope=openid%20mail" +
      "&state=x%2By&code_challenge=abc_-&code_challenge_method=S256&nonce=n&prompt=consent";
    expect(decisionBody(search, true)).toEqual({
      response_type: "code",
      client_id: "uwu-abc",
      redirect_uri: "http://127.0.0.1:5000/",
      scope: "openid mail",
      state: "x+y",
      code_challenge: "abc_-",
      code_challenge_method: "S256",
      nonce: "n",
      prompt: "consent",
      approve: true,
    });
  });

  it("lets nothing in the address decide for the person", () => {
    expect(decisionBody("?client_id=a&approve=true", false)).toEqual({ client_id: "a", approve: false });
  });
});

describe("knownScopes", () => {
  it("keeps the ones it can explain, in a fixed order", () => {
    expect(knownScopes(["openid", "smtp", "something", "mail"])).toEqual(["mail", "smtp", "openid"]);
    expect(knownScopes(["openid", "maskedemail"])).toEqual(["maskedemail", "openid"]);
    expect(knownScopes(["openid", "app-password"])).toEqual(["app-password", "openid"]);
  });
});

describe("masksOnly", () => {
  it("is true only when nothing of the mailbox comes along", () => {
    expect(masksOnly(["maskedemail"])).toBe(true);
    expect(masksOnly(["openid", "maskedemail"])).toBe(true);
    expect(masksOnly(["mail", "maskedemail"])).toBe(false);
    expect(masksOnly(["smtp", "maskedemail"])).toBe(false);
    expect(masksOnly(["openid"])).toBe(false);
  });
});

describe("appPasswordOnly", () => {
  it("is true only when the app asks for an app password and no protocol itself", () => {
    expect(appPasswordOnly(["app-password"])).toBe(true);
    expect(appPasswordOnly(["openid", "app-password"])).toBe(true);
    expect(appPasswordOnly(["mail", "app-password"])).toBe(false);
    expect(appPasswordOnly(["maskedemail", "app-password"])).toBe(false);
    expect(appPasswordOnly(["openid"])).toBe(false);
  });
});
