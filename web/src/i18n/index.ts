import i18n from "i18next";
import { useEffect } from "react";
import { initReactI18next, useTranslation } from "react-i18next";
import { useBrand } from "@/state/brand";
import { usePrefs, type LanguageSetting } from "@/state/prefs";
import deNeutral from "./locales/de/neutral.json";
import dePlayful from "./locales/de/playful.json";
import enNeutral from "./locales/en/neutral.json";
import enPlayful from "./locales/en/playful.json";
import frNeutral from "./locales/fr/neutral.json";
import frPlayful from "./locales/fr/playful.json";
import jaNeutral from "./locales/ja/neutral.json";
import jaPlayful from "./locales/ja/playful.json";
import nlNeutral from "./locales/nl/neutral.json";
import nlPlayful from "./locales/nl/playful.json";
import zhNeutral from "./locales/zh/neutral.json";
import zhPlayful from "./locales/zh/playful.json";

export const LANGUAGES = ["de", "en", "fr", "nl", "ja", "zh"] as const;
export type Language = (typeof LANGUAGES)[number];

/** Every language in its own words, so people find theirs whatever the page is in now. */
export const LANGUAGE_NAMES: Record<Language, string> = {
  de: "Deutsch",
  en: "English",
  fr: "Français",
  nl: "Nederlands",
  ja: "日本語",
  zh: "简体中文",
};

/** What `<html lang>` says; Chinese is written in simplified characters. */
export const HTML_LANG: Record<Language, string> = { de: "de", en: "en", fr: "fr", nl: "nl", ja: "ja", zh: "zh-Hans" };

export const resources = {
  en: { neutral: enNeutral, playful: enPlayful },
  de: { neutral: deNeutral, playful: dePlayful },
  fr: { neutral: frNeutral, playful: frPlayful },
  nl: { neutral: nlNeutral, playful: nlPlayful },
  ja: { neutral: jaNeutral, playful: jaPlayful },
  zh: { neutral: zhNeutral, playful: zhPlayful },
} as const;

/** The first of the browser's languages the portal speaks, English when it speaks none of them. */
export function browserLanguage(preferred: readonly string[] = navigator.languages ?? [navigator.language]): Language {
  for (const tag of preferred) {
    const base = tag.toLowerCase().split(/[-_]/)[0];
    const found = LANGUAGES.find((language) => language === base);
    if (found) return found;
  }
  return "en";
}

export function resolveLanguage(setting: LanguageSetting): Language {
  return setting === "system" ? browserLanguage() : setting;
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
    // `{{brand}}` is the server's name in every text, UwUMail unless an admin chose another.
    interpolation: { escapeValue: false, defaultVariables: { brand: useBrand.getState().name } },
    postProcess: ["kaomoji"],
  });

/** `t` for the active tone. Every component should use this instead of useTranslation. */
export function useT() {
  const tone = usePrefs((s) => s.tone);
  // Without the mascot there is no playful tone, whatever someone chose before.
  const mascot = useBrand((s) => s.mascot);
  return useTranslation(mascot ? tone : "neutral");
}

/** Keeps `{{brand}}` in step with the server's name, and redraws the texts when it changes. */
export function useApplyBrandName() {
  const name = useBrand((s) => s.name);
  useEffect(() => {
    const interpolation = i18n.options.interpolation ?? {};
    if (interpolation.defaultVariables?.brand === name) return;
    i18n.options.interpolation = {
      ...interpolation,
      defaultVariables: { ...interpolation.defaultVariables, brand: name },
    };
    // Changing to the same language makes every translated text render again.
    void i18n.changeLanguage(i18n.language);
  }, [name]);
}

/** Follows the language preference and keeps <html lang> in sync. */
export function useApplyLanguage() {
  const setting = usePrefs((s) => s.language);
  useEffect(() => {
    const language = resolveLanguage(setting);
    if (i18n.language !== language) void i18n.changeLanguage(language);
    document.documentElement.lang = HTML_LANG[language];
  }, [setting]);
}

export { i18n };
