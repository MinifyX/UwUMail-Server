import { beforeAll, describe, expect, it } from "vitest";
import { i18n } from "@/i18n";
import type { LeftOutMessage } from "@/lib/api";
import { leftOutParts, leftOutReason } from "./LeftOut";

const t = i18n.t.bind(i18n) as Parameters<typeof leftOutParts>[0];

beforeAll(async () => {
  await i18n.changeLanguage("de");
});

const counts = (known: number, tooLarge: number, unreadable: number, skipped = known + tooLarge + unreadable) => ({
  messagesSkipped: skipped,
  messagesKnown: known,
  messagesTooLarge: tooLarge,
  messagesUnreadable: unreadable,
});

describe("leftOutParts", () => {
  it("names each reason that happened, and only those", () => {
    expect(leftOutParts(t, counts(3, 2, 1))).toEqual(["3 schon da", "2 zu groß", "1 nicht lesbar"]);
    expect(leftOutParts(t, counts(0, 2, 0))).toEqual(["2 zu groß"]);
    expect(leftOutParts(t, counts(0, 0, 0))).toEqual([]);
  });

  it("keeps what older moves counted only in the total", () => {
    expect(leftOutParts(t, counts(1, 0, 0, 13))).toEqual(["1 schon da", "12 früher ausgelassen"]);
  });
});

describe("leftOutReason", () => {
  const message = (reason: LeftOutMessage["reason"]): LeftOutMessage => ({
    folder: "INBOX",
    uid: 7,
    reason,
    from: "",
    subject: "",
    date: null,
    size: 80 * 1024 * 1024,
    recordedAt: 0,
  });

  it("says the limit for messages too large", () => {
    expect(leftOutReason(t, message("tooLarge"), 50 * 1024 * 1024, "de")).toContain("50");
    expect(leftOutReason(t, message("tooLarge"), 0, "de")).toBe("größer, als dieser Server annimmt");
    expect(leftOutReason(t, message("unreadable"), 0, "de")).toBe("zu verschachtelt, um sicher gelesen zu werden");
    expect(leftOutReason(t, message("known"), 0, "de")).toBe("schon in diesem Ordner hier");
  });
});
