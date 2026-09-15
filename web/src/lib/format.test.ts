import type { TFunction } from "i18next";
import { describe, expect, it } from "vitest";
import { formatBytes, formatDuration } from "./format";

const t = ((key: string, options: { count: number }) =>
  `${options.count} ${key.split(".")[1]}`) as unknown as TFunction;

describe("format", () => {
  it("picks a readable unit", () => {
    expect(formatBytes(0, "en")).toBe("0 byte");
    expect(formatBytes(1536, "en")).toBe("1.5 kB");
    // German puts a no-break space between number and unit.
    expect(formatBytes(5 * 1024 ** 3, "de").replace(/\s/g, " ")).toBe("5 GB");
  });

  it("shows the two largest parts of a duration", () => {
    expect(formatDuration(59, t)).toBe("0 minutes");
    expect(formatDuration(3 * 3600 + 120, t)).toBe("3 hours, 2 minutes");
    expect(formatDuration(2 * 86_400 + 5 * 3600 + 60, t)).toBe("2 days, 5 hours");
  });
});
