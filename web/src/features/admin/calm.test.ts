import { describe, expect, it } from "vitest";
import type { AdminAlert, Health } from "@/lib/api";
import { calmItems, calmLevel } from "./calm";
import { adminViewOf, alertMailsOf, inCalmNav } from "./adminPrefs";

const health: Health = {
  level: "warning",
  checkedAt: 1,
  areas: [
    {
      area: "certificate",
      level: "warning",
      findings: [{ code: "certExpiresSoon", level: "warning", params: { days: 9 } }],
    },
    { area: "dns", level: "unknown", findings: [{ code: "dnsPending", level: "unknown", params: { count: 1 } }] },
    { area: "storage", level: "ok", findings: [{ code: "diskOk", level: "ok" }] },
  ],
};

const alert = (kind: string, code: string, level: AdminAlert["level"]): AdminAlert => ({
  id: 7,
  kind,
  key: code,
  code,
  level,
  params: {},
  link: "/admin/backups",
  firstSeen: 1,
  lastSeen: 1,
  resolvedAt: null,
  notifiedAt: null,
  notifiedLevel: null,
  acknowledgedAt: null,
  acknowledgedBy: null,
});

describe("calm view", () => {
  it("lists only what is yellow or red, the red first", () => {
    const alerts = {
      open: [alert("backup", "backupFailed", "problem"), alert("update", "updateAvailable", "info")],
      resolved: [],
    };
    const items = calmItems(health, alerts);
    expect(items.map((item) => item.level)).toEqual(["problem", "warning"]);
    expect(items[0]?.source).toBe("alert");
    expect(calmLevel(health, alerts)).toBe("problem");
  });

  it("does not repeat what the health overview already shows", () => {
    const alerts = { open: [alert("certificate", "certExpiresSoon", "warning")], resolved: [] };
    expect(calmItems(health, alerts)).toHaveLength(1);
    expect(calmLevel(health, alerts)).toBe("warning");
    expect(calmLevel(undefined, alerts)).toBe("unknown");
  });

  it("keeps everything for admins who never chose", () => {
    expect(adminViewOf({})).toBe("full");
    expect(adminViewOf({ adminView: "simple" })).toBe("simple");
    expect(alertMailsOf({ adminAlerts: "problems" })).toBe("problems");
    expect(alertMailsOf({ adminAlerts: "loud" })).toBe("all");
  });

  it("keeps the overview and the people in sight and folds the rest away", () => {
    expect(["/admin", "/admin/people"].every(inCalmNav)).toBe(true);
    expect(["/admin/queue", "/admin/spam", "/admin/settings", "/admin/logs"].some(inCalmNav)).toBe(false);
  });
});
