import { describe, expect, it } from "vitest";
import type { Form } from "@/features/settings/SettingsPage";
import { metricsCanSave, newMetricsToken, parseNetworks, tokenState } from "./MetricsCard";

/** A settings form as the metrics card sees it: what is stored, and what is drafted on top. */
function form(stored: Record<string, { value: unknown; set?: boolean }>, draft: Record<string, unknown> = {}): Form {
  const pending = Object.fromEntries(
    Object.entries(draft).filter(([key, value]) => value !== undefined && (value !== null || stored[key]?.set)),
  );
  return {
    setting: (key) => {
      const entry = stored[key];
      return entry && { key, value: entry.value, set: entry.set ?? entry.value !== null, source: "default" };
    },
    value: (key) => (key in draft ? draft[key] : stored[key]?.value),
    locked: () => false,
    set: () => {},
    pending,
  };
}

const off = {
  "metrics.enabled": { value: false },
  "metrics.token": { value: null, set: false },
  "metrics.allowed_networks": { value: [] },
};

describe("metrics settings", () => {
  it("never switch the metrics on without a token or a network", () => {
    expect(metricsCanSave(form(off))).toBe(true);
    expect(metricsCanSave(form(off, { "metrics.enabled": true }))).toBe(false);
    expect(metricsCanSave(form(off, { "metrics.enabled": true, "metrics.token": newMetricsToken() }))).toBe(true);
    expect(metricsCanSave(form(off, { "metrics.enabled": true, "metrics.allowed_networks": ["10.0.0.0/8"] }))).toBe(
      true,
    );
  });

  it("know a stored token without ever seeing it, and notice when it is removed", () => {
    const stored = { ...off, "metrics.enabled": { value: true }, "metrics.token": { value: null, set: true } };
    expect(tokenState(form(stored))).toEqual({ kind: "stored" });
    expect(metricsCanSave(form(stored))).toBe(true);
    expect(tokenState(form(stored, { "metrics.token": null }))).toEqual({ kind: "none" });
    expect(metricsCanSave(form(stored, { "metrics.token": null }))).toBe(false);
    expect(tokenState(form(stored, { "metrics.token": "fresh" }))).toEqual({ kind: "fresh", token: "fresh" });
  });

  it("make long random tokens and read networks one per line", () => {
    const token = newMetricsToken();
    expect(token).toMatch(/^[a-zA-Z2-9]{32}$/);
    expect(newMetricsToken()).not.toBe(token);
    expect(parseNetworks(" 192.0.2.0/24\n\n2001:db8::/32, 198.51.100.7 ")).toEqual([
      "192.0.2.0/24",
      "2001:db8::/32",
      "198.51.100.7",
    ]);
  });
});
