import { describe, expect, it } from "vitest";
import { codeSettingsFromPreferences, codeSettingsPatch, DEFAULT_CODE_SETTINGS } from "./codeSettings";

describe("codeSettingsFromPreferences", () => {
  it("starts with detection on and the trash after 24 hours", () => {
    expect(codeSettingsFromPreferences(undefined)).toEqual({ detect: true, cleanup: "24h" });
    expect(DEFAULT_CODE_SETTINGS).toEqual({ detect: true, cleanup: "24h" });
  });

  it("keeps known values and falls back for the rest", () => {
    expect(
      codeSettingsFromPreferences({ mailVerificationHeuristics: "off", mailVerificationCleanup: "7d", extra: 1 }),
    ).toEqual({ detect: false, cleanup: "7d" });
    expect(codeSettingsFromPreferences({ mailVerificationHeuristics: "on", mailVerificationCleanup: "now" })).toEqual(
      DEFAULT_CODE_SETTINGS,
    );
  });
});

describe("codeSettingsPatch", () => {
  it("sends only what changed, in the server's values", () => {
    expect(codeSettingsPatch({ detect: false })).toEqual({ mailVerificationHeuristics: "off" });
    expect(codeSettingsPatch({ cleanup: "immediately" })).toEqual({ mailVerificationCleanup: "immediately" });
  });
});
