import { describe, expect, it } from "vitest";
import { keepKaomojiTogether } from "./index";

const WORD_JOINER = String.fromCharCode(0x2060);

describe("keepKaomojiTogether", () => {
  it("joins the characters of a kaomoji", () => {
    const text = keepKaomojiTogether("Ich passe mit auf (=^･ω･^=)");
    expect(text.startsWith("Ich passe mit auf (")).toBe(true);
    expect(text.split(WORD_JOINER).join("")).toBe("Ich passe mit auf (=^･ω･^=)");
    expect(text.includes(`･${WORD_JOINER}^`)).toBe(true);
  });

  it("leaves plain brackets alone", () => {
    expect(keepKaomojiTogether("Port 465 (TLS) oder 587 (STARTTLS)")).toBe("Port 465 (TLS) oder 587 (STARTTLS)");
    expect(keepKaomojiTogether("Hallo (lange Klammer mit Leerzeichen ✉)")).not.toContain(WORD_JOINER);
  });
});
