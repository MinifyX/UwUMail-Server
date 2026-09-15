import { describe, expect, it } from "vitest";
import deNeutral from "./locales/de/neutral.json";
import dePlayful from "./locales/de/playful.json";
import enNeutral from "./locales/en/neutral.json";
import enPlayful from "./locales/en/playful.json";

type Tree = { [key: string]: string | Tree };

function keys(tree: Tree, prefix = ""): string[] {
  return Object.entries(tree).flatMap(([key, value]) =>
    typeof value === "string" ? [`${prefix}${key}`] : keys(value, `${prefix}${key}.`),
  );
}

describe("locales", () => {
  it("German and English neutral strings have the same keys", () => {
    expect(keys(deNeutral).sort()).toEqual(keys(enNeutral).sort());
  });

  // i18next checks the playful namespace in every language before falling back
  // to neutral, so a playful key missing in one language would show the other
  // language's text.
  it("German and English playful strings have the same keys", () => {
    expect(keys(dePlayful).sort()).toEqual(keys(enPlayful).sort());
  });

  it("every playful key overrides an existing neutral key", () => {
    const neutral = new Set(keys(enNeutral));
    expect(keys(enPlayful).filter((key) => !neutral.has(key))).toEqual([]);
  });
});
