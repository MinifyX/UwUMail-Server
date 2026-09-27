import { describe, expect, it } from "vitest";
import { decisionBody, knownScopes } from "./authorize";

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
  });
});
