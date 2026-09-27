import { describe, expect, it } from "vitest";
import { providerOf } from "./MovingPage";

describe("providerOf", () => {
  it("knows the providers that want something done first", () => {
    expect(providerOf("mini@gmail.com")).toBe("gmail");
    expect(providerOf("Mini@GoogleMail.com ")).toBe("gmail");
    expect(providerOf("mini@outlook.de")).toBe("outlook");
    expect(providerOf("mini@hotmail.co.uk")).toBe("outlook");
    expect(providerOf("mini@live.com")).toBe("outlook");
    expect(providerOf("mini@gmx.net")).toBe("gmx");
    expect(providerOf("mini@web.de")).toBe("webde");
  });

  it("says nothing about everybody else", () => {
    expect(providerOf("mini@example.com")).toBeNull();
    expect(providerOf("mini@notgmail.com")).toBeNull();
    expect(providerOf("mini")).toBeNull();
    expect(providerOf("")).toBeNull();
  });
});
