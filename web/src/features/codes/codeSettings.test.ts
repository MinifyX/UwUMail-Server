import { describe, expect, it } from "vitest";
import { DEFAULT_CODE_SETTINGS, sanitizeCodeSettings } from "./codeSettings";

describe("sanitizeCodeSettings", () => {
  it("starts with detection on and the trash after 24 hours", () => {
    expect(sanitizeCodeSettings(undefined)).toEqual({ detect: true, cleanup: "24h" });
    expect(DEFAULT_CODE_SETTINGS).toEqual({ detect: true, cleanup: "24h" });
  });

  it("keeps known values and drops the rest", () => {
    expect(sanitizeCodeSettings({ detect: false, cleanup: "7d", extra: 1 })).toEqual({ detect: false, cleanup: "7d" });
    expect(sanitizeCodeSettings({ detect: "yes", cleanup: "1h" })).toEqual(DEFAULT_CODE_SETTINGS);
  });
});
