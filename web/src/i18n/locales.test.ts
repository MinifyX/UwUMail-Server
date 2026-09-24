import { describe, expect, it } from "vitest";
import { LANGUAGES, resources, type Language } from "./index";

type Tree = { [key: string]: string | Tree };

function entries(tree: Tree, prefix = ""): [string, string][] {
  return Object.entries(tree).flatMap(([key, value]) =>
    typeof value === "string" ? [[`${prefix}${key}`, value] as [string, string]] : entries(value, `${prefix}${key}.`),
  );
}

const PLURAL = /_(zero|one|two|few|many|other)$/;

/** The keys without their plural endings: every language counts differently. */
function bases(tree: Tree): string[] {
  return [...new Set(entries(tree).map(([key]) => key.replace(PLURAL, "")))].sort();
}

function placeholders(text: string): string[] {
  return [...text.matchAll(/\{\{\s*([\w.]+)[^}]*\}\}/g)].map((match) => match[1]!).sort();
}

const neutral = (language: Language) => resources[language].neutral as unknown as Tree;
const playful = (language: Language) => resources[language].playful as unknown as Tree;

describe.each(LANGUAGES.filter((language) => language !== "en"))("%s", (language) => {
  it("has every text English has, and no others", () => {
    expect(bases(neutral(language))).toEqual(bases(neutral("en")));
  });

  it("overrides the same texts in the playful tone, each one existing in the neutral tone", () => {
    expect(bases(playful(language))).toEqual(bases(playful("en")));
    const known = new Set(entries(neutral(language)).map(([key]) => key));
    expect(entries(playful(language)).filter(([key]) => !known.has(key))).toEqual([]);
  });

  it("keeps every placeholder", () => {
    const own = new Map(entries(neutral(language)));
    const missing = entries(neutral("en"))
      // A singular may say "one" instead of {{count}}; the other forms have to keep it.
      .filter(([key]) => !key.endsWith("_one"))
      .flatMap(([key, text]) => {
        const translated = own.get(key) ?? own.get(key.replace(PLURAL, "_other")) ?? own.get(key.replace(PLURAL, ""));
        if (translated === undefined) return [];
        const lost = placeholders(text).filter((name) => !placeholders(translated).includes(name));
        return lost.length > 0 ? [`${key}: ${lost.join(", ")}`] : [];
      });
    expect(missing).toEqual([]);
  });

  it("has the plural forms the language needs", () => {
    const categories = new Intl.PluralRules(language).resolvedOptions().pluralCategories;
    const keys = new Set(entries(neutral(language)).map(([key]) => key));
    const plurals = new Set(
      entries(neutral("en"))
        .map(([key]) => key)
        .filter((key) => key.endsWith("_other"))
        .map((key) => key.replace(PLURAL, "")),
    );
    const missing = [...plurals].flatMap((base) =>
      categories
        .filter((category) => category === "other" || category === "one")
        .flatMap((category) =>
          // Languages without a singular form only need "other".
          categories.includes(category) && !keys.has(`${base}_${category}`) && !keys.has(base)
            ? [`${base}_${category}`]
            : [],
        ),
    );
    expect(missing).toEqual([]);
  });
});

describe("en", () => {
  it("every playful key overrides an existing neutral key", () => {
    const known = new Set(entries(neutral("en")).map(([key]) => key));
    expect(entries(playful("en")).filter(([key]) => !known.has(key))).toEqual([]);
  });
});
