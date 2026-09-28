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

// The change log's actions of 0.14 and 0.15; each one has its own sentence instead of the raw key.
const ACTIONS = [
  "account.authSource",
  "account.oauthRevoked",
  "alert.acknowledge",
  "domain.maskedAddresses",
  "domain.maskedPolicy",
  "domain.kind",
  "account.maskedPolicy",
  "group.create",
  "group.update",
  "group.remove",
  "sharedMailbox.create",
  "sharedMailbox.members",
  "sharedMailbox.convert",
  "sharedMailbox.end",
  "person.picture",
  "person.pictureRemoved",
  "person.pictureVisibility",
  "group.picture",
  "group.pictureRemoved",
  "group.pictureVisibility",
  "domain.logo",
  "domain.logoRemoved",
  "domain.publicPictures",
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

  it("says what changed about masked addresses", () => {
    const t = i18n.getFixedT("en", "neutral");
    const kind = record("domain.kind", { kind: "mail", removedFromDomains: ["example.org"], removedFromAccounts: [] });
    expect(detailText(kind, t, "en")).toBe("now a mail domain · taken out of 1 masked address setting");
    expect(detailText(record("domain.create", { kind: "mail" }), t, "en")).toBe("");
    expect(detailText(record("domain.create", { kind: "masked" }), t, "en")).toBe("only for masked addresses");
    const policy = record("domain.maskedPolicy", { mode: "dedicated", maskedDomains: ["masked.example"] });
    expect(detailText(policy, t, "en")).toBe("masked addresses: Masked-only domains");
    expect(detailText(record("account.maskedPolicy", { mode: null }), t, "en")).toBe("masked addresses: as the domain");
  });
});
