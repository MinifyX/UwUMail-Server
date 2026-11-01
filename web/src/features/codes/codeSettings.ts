import { create } from "zustand";

/** What happens to a mail once its code has expired: it stays, or goes to the trash after a while. */
export type CodeCleanup = "off" | "now" | "24h" | "7d";

export const CODE_CLEANUPS: readonly CodeCleanup[] = ["off", "now", "24h", "7d"];

export interface CodeSettings {
  /** Look for codes in German and English mails without a signed Verification-Info header. */
  detect: boolean;
  cleanup: CodeCleanup;
}

export const DEFAULT_CODE_SETTINGS: CodeSettings = { detect: true, cleanup: "24h" };

/** Keeps only known fields with allowed values. */
export function sanitizeCodeSettings(raw: unknown): CodeSettings {
  const value = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  return {
    detect: typeof value.detect === "boolean" ? value.detect : DEFAULT_CODE_SETTINGS.detect,
    cleanup: CODE_CLEANUPS.includes(value.cleanup as CodeCleanup)
      ? (value.cleanup as CodeCleanup)
      : DEFAULT_CODE_SETTINGS.cleanup,
  };
}

/*
 * TODO(main session): the server has no API for this yet. Proposed: the account preferences
 * (`PATCH /api/account/preferences`, read back in `session.preferences`) get
 *   verificationCodes.detect: boolean            (default true)
 *   verificationCodes.cleanup: "off" | "now" | "24h" | "7d"   (default "24h"; expired → trash)
 * Until then the choice lives in this browser only, and the page says so.
 */
const STORAGE_KEY = "uwumail-portal-codes";

function load(): CodeSettings {
  try {
    return sanitizeCodeSettings(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}"));
  } catch {
    return DEFAULT_CODE_SETTINGS;
  }
}

interface CodeSettingsState extends CodeSettings {
  set: (changes: Partial<CodeSettings>) => void;
}

export const useCodeSettings = create<CodeSettingsState>((set) => ({
  ...load(),
  set: (changes) =>
    set((state) => {
      const next = sanitizeCodeSettings({ detect: state.detect, cleanup: state.cleanup, ...changes });
      try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
      } catch {
        // Private windows may refuse storage; the choice then holds for this visit.
      }
      return next;
    }),
}));
