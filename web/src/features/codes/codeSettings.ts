/**
 * Verification code settings are account preferences (`PATCH /api/account/preferences`, read back in
 * `session.preferences`). The server accepts exactly these values (routes/account.rs) and mirrors them
 * to apps as JMAP UserSettings `verificationHeuristics` / `verificationCleanup` (docs/verification-codes.md).
 */

/** When a mail whose code expired moves from the inbox to the trash. */
export const CODE_CLEANUPS = ["off", "immediately", "24h", "7d"] as const;
export type CodeCleanup = (typeof CODE_CLEANUPS)[number];

export interface CodeSettings {
  /** Also guess codes in mail without a signed Verification-Info header. */
  detect: boolean;
  cleanup: CodeCleanup;
}

/** What the server uses when nobody chose. */
export const DEFAULT_CODE_SETTINGS: CodeSettings = { detect: true, cleanup: "24h" };

/** Reads the settings from the stored preferences; unknown or missing values mean the default. */
export function codeSettingsFromPreferences(preferences: Record<string, unknown> | undefined): CodeSettings {
  const cleanup = preferences?.mailVerificationCleanup;
  return {
    detect: preferences?.mailVerificationHeuristics !== "off",
    cleanup: CODE_CLEANUPS.includes(cleanup as CodeCleanup) ? (cleanup as CodeCleanup) : DEFAULT_CODE_SETTINGS.cleanup,
  };
}

/** The preference fields to send for a change. */
export function codeSettingsPatch(changes: Partial<CodeSettings>): Record<string, string> {
  const patch: Record<string, string> = {};
  if (changes.detect !== undefined) patch.mailVerificationHeuristics = changes.detect ? "on" : "off";
  if (changes.cleanup !== undefined) patch.mailVerificationCleanup = changes.cleanup;
  return patch;
}
