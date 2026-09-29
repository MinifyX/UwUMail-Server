import { describe, expect, it } from "vitest";
import type { FetchAccountInfo } from "@/lib/api";
import {
  keepDraft,
  looksLikeAddress,
  providerOfAddress,
  readSignInReturn,
  signInError,
  signInNeed,
  takeDraft,
  withoutSignInReturn,
} from "./signIn";

describe("providerOfAddress", () => {
  it("knows Microsoft's and Google's own domains, as the server does", () => {
    for (const address of [
      "mini@outlook.com",
      "mini@hotmail.de",
      "mini@hotmail.co.uk",
      "Mini@LIVE.DE",
      "mini@msn.com",
    ]) {
      expect(providerOfAddress(address), address).toBe("microsoft");
    }
    expect(providerOfAddress("mini@gmail.com")).toBe("google");
    expect(providerOfAddress("mini@googlemail.com")).toBe("google");
  });

  it("leaves everything else to the server", () => {
    for (const address of [
      "mini@example.com",
      "mini@hotmail.example.com",
      "mini@livemail.test",
      "mini@gmail.de",
      "mini",
    ]) {
      expect(providerOfAddress(address), address).toBeNull();
    }
  });

  it("asks the server only about complete addresses", () => {
    expect(looksLikeAddress("mini@example.com")).toBe(true);
    expect(looksLikeAddress("mini@example")).toBe(false);
    expect(looksLikeAddress("mini@")).toBe(false);
  });
});

describe("Google's way back", () => {
  it("reads the sign-in to finish, or why there is none", () => {
    expect(readSignInReturn("?oauth=Abc_def-12345678")).toEqual({ flow: "Abc_def-12345678" });
    expect(readSignInReturn("?oauthError=declined")).toEqual({ error: "declined" });
    expect(readSignInReturn("?oauthError=<script>")).toEqual({ error: "failed" });
    expect(readSignInReturn("?oauth=../../x")).toBeNull();
    expect(readSignInReturn("")).toBeNull();
  });

  it("takes its parameters out of the address and leaves the rest", () => {
    expect(withoutSignInReturn("?oauth=abcdefgh1234&tab=2")).toBe("?tab=2");
    expect(withoutSignInReturn("?oauthError=expired")).toBe("");
  });

  it("maps unknown failure codes to the general one", () => {
    expect(signInError("signInRefused")).toBe("signInRefused");
    expect(signInError("somethingNew")).toBe("failed");
    expect(signInError(undefined)).toBe("failed");
  });

  it("keeps the dialog's choices for the way there and back, once", () => {
    const map = new Map<string, string>();
    const storage = {
      setItem: (key: string, value: string) => void map.set(key, value),
      getItem: (key: string) => map.get(key) ?? null,
      removeItem: (key: string) => void map.delete(key),
    };
    keepDraft({ afterFetch: "delete", fetchJunk: false, intervalSecs: 900, takeExisting: true }, storage);
    expect(takeDraft(storage)).toEqual({
      afterFetch: "delete",
      fetchJunk: false,
      intervalSecs: 900,
      takeExisting: true,
    });
    expect(takeDraft(storage)).toBeNull();
    map.set("uwumail.fetch.signInDraft", "{not json");
    expect(takeDraft(storage)).toBeNull();
    expect(takeDraft(null)).toBeNull();
  });
});

describe("signInNeed", () => {
  const base = {
    auth: "password",
    loginExpired: false,
    passwordRefused: false,
    signIn: null,
  } as unknown as FetchAccountInfo;

  it("puts an ended sign-in first, then a refused password, then the offer to switch", () => {
    expect(signInNeed({ ...base, auth: "microsoft", loginExpired: true })).toEqual({
      kind: "expired",
      provider: "microsoft",
    });
    expect(signInNeed({ ...base, passwordRefused: true, signIn: "microsoft" })).toEqual({ kind: "passwordRefused" });
    expect(signInNeed({ ...base, signIn: "google" })).toEqual({ kind: "canSwitch", provider: "google" });
    expect(signInNeed(base)).toBeNull();
    expect(signInNeed({ ...base, auth: "google" })).toBeNull();
  });
});
