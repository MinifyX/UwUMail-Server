import i18n from "i18next";
import { useEffect } from "react";
import { initReactI18next, useTranslation } from "react-i18next";
import { usePrefs, type LanguageSetting } from "@/state/prefs";
import deNeutral from "./locales/de/neutral.json";
import dePlayful from "./locales/de/playful.json";
import enNeutral from "./locales/en/neutral.json";
import enPlayful from "./locales/en/playful.json";

export type Language = "de" | "en";

export const resources = {
  en: { neutral: enNeutral, playful: enPlayful },
  de: { neutral: deNeutral, playful: dePlayful },
} as const;

export function resolveLanguage(setting: LanguageSetting): Language {
  if (setting !== "system") return setting;
  return navigator.language.toLowerCase().startsWith("de") ? "de" : "en";
}

const WORD_JOINER = String.fromCharCode(0x2060);
const KAOMOJI = /\([^()\s]{2,14}\)/g;

/**
 * Browsers happily wrap a line in the middle of (=^･ω･^=). A word joiner between
 * every character of a kaomoji (brackets with at least one non-ASCII character)
 * keeps each face on one line.
 */
export function keepKaomojiTogether(text: string): string {
  return text.replace(KAOMOJI, (face) => (/[^\x20-\x7e]/.test(face) ? [...face].join(WORD_JOINER) : face));
}

void i18n
  .use(initReactI18next)
  .use({ type: "postProcessor", name: "kaomoji", process: (value: string) => keepKaomojiTogether(value) })
  .init({
    resources,
    lng: resolveLanguage(usePrefs.getState().language),
    fallbackLng: "en",
    ns: ["neutral", "playful"],
    defaultNS: "neutral",
    // Playful strings only override some keys; everything else comes from neutral.
    fallbackNS: "neutral",
    interpolation: { escapeValue: false },
    postProcess: ["kaomoji"],
  });

/** `t` for the active tone. Every component should use this instead of useTranslation. */
export function useT() {
  const tone = usePrefs((s) => s.tone);
  return useTranslation(tone);
}

/** Follows the language preference and keeps <html lang> in sync. */
export function useApplyLanguage() {
  const setting = usePrefs((s) => s.language);
  useEffect(() => {
    const language = resolveLanguage(setting);
    if (i18n.language !== language) void i18n.changeLanguage(language);
    document.documentElement.lang = language;
  }, [setting]);
}

export { i18n };
