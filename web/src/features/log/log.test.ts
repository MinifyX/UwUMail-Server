import i18n from "i18next";
import { describe, expect, it } from "vitest";
import { LANGUAGES } from "@/i18n";
import type { AuditRecord } from "@/lib/api";
import { describe as describeRecord, detailText } from "./LogPage";

const record = (action: string, details: Record<string, unknown> = {}): AuditRecord => ({
  id: 1,
  at: 1,
  actor: "mini@example.org",
  action,
  target: "vorstand@example.org",
  details,
  ip: "",
});

// The change log's actions of 0.14; each one has its own sentence instead of the raw key.
const ACTIONS = [
  "account.authSource",
  "account.oauthRevoked",
  "alert.acknowledge",
  "domain.maskedAddresses",
  "group.create",
  "group.update",
  "group.remove",
  "sharedMailbox.create",
  "sharedMailbox.members",
];

describe("the change log", () => {
  it.each(LANGUAGES)("names every action in %s", (language) => {
    const t = i18n.getFixedT(language, "neutral");
    for (const action of ACTIONS) {
      const text = describeRecord(record(action), t);
      expect(text, action).not.toContain(action);
      expect(text, action).toContain("vorstand@example.org");
    }
  });

  it("says what was switched and where passwords are checked now", () => {
    const t = i18n.getFixedT("en", "neutral");
    expect(detailText(record("domain.maskedAddresses", { on: false }), t, "en")).toBe("switched off");
    expect(detailText(record("account.authSource", { source: "ldap" }), t, "en")).toBe("now: At the LDAP directory");
    expect(detailText(record("account.oauthRevoked", { name: "Mail" }), t, "en")).toBe("app “Mail”");
  });
});
