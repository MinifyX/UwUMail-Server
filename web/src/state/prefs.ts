import { create } from "zustand";

export type LanguageSetting = "system" | "de" | "en";
export type Tone = "playful" | "neutral";
export type Mode = "simple" | "pro";
export type ThemeSetting = "system" | "light" | "dark";
export type MotionSetting = "system" | "on" | "off";

export interface Prefs {
  language: LanguageSetting;
  tone: Tone;
  mode: Mode;
  theme: ThemeSetting;
  motion: MotionSetting;
}

export const DEFAULT_PREFS: Prefs = {
  language: "system",
  tone: "playful",
  mode: "simple",
  theme: "system",
  motion: "system",
};

const ALLOWED: { [K in keyof Prefs]: readonly Prefs[K][] } = {
  language: ["system", "de", "en"],
  tone: ["playful", "neutral"],
  mode: ["simple", "pro"],
  theme: ["system", "light", "dark"],
  motion: ["system", "on", "off"],
};

const STORAGE_KEY = "uwumail-portal-prefs";

/** Keeps only known keys with allowed values, e.g. from the server or localStorage. */
export function sanitize(raw: unknown): Partial<Prefs> {
  if (!raw || typeof raw !== "object") return {};
  const result: Partial<Record<keyof Prefs, string>> = {};
  for (const key of Object.keys(ALLOWED) as (keyof Prefs)[]) {
    const value = (raw as Record<string, unknown>)[key];
    if (typeof value === "string" && (ALLOWED[key] as readonly string[]).includes(value)) result[key] = value;
  }
  return result as Partial<Prefs>;
}

function load(): Prefs {
  try {
    return { ...DEFAULT_PREFS, ...sanitize(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}")) };
  } catch {
    return DEFAULT_PREFS;
  }
}

interface PrefsState extends Prefs {
  /** Applies preferences locally, e.g. the ones stored on the server after login. */
  apply: (prefs: Partial<Prefs>) => void;
}

/** Portal preferences. The browser remembers them for the login page; logged in, the server has the say. */
export const usePrefs = create<PrefsState>((set) => ({
  ...load(),
  apply: (prefs) =>
    set((state) => {
      const next = { ...state, ...sanitize(prefs) };
      try {
        const { language, tone, mode, theme, motion } = next;
        localStorage.setItem(STORAGE_KEY, JSON.stringify({ language, tone, mode, theme, motion }));
      } catch {
        // Private windows may refuse storage; the preferences still apply for now.
      }
      return next;
    }),
}));
