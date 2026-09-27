import { describe, expect, it } from "vitest";
import type { StatsView } from "@/lib/api";
import { formatPeriod, niceBytesMax, niceMax, REFUSED_KEYS, series, total } from "./series";

const view: StatsView = {
  range: "days",
  periods: [
    { period: "2026-09-26", values: {} },
    {
      period: "2026-09-27",
      values: { "refused.spam": 3, "refused.virus": 1, "gauge.storageBytes": 2048, "mail.received": 7 },
    },
  ],
  totals: { "refused.spam": 3, "refused.virus": 1, "mail.received": 7 },
};

describe("statistics", () => {
  it("adds up counters and leaves gauges without a reading empty", () => {
    expect(series(view, REFUSED_KEYS).map((point) => point.value)).toEqual([0, 4]);
    expect(series(view, ["gauge.storageBytes"]).map((point) => point.value)).toEqual([null, 2048]);
    expect(total(view, REFUSED_KEYS)).toBe(4);
    // Greylisted mail mostly comes back and is received then; it is not counted as turned away.
    expect(REFUSED_KEYS).not.toContain("refused.greylisted");
  });

  it("rounds the axis up to a number people like", () => {
    expect(niceMax(0)).toBe(1);
    expect(niceMax(7)).toBe(10);
    expect(niceMax(13)).toBe(20);
    expect(niceMax(420)).toBe(500);
    expect(niceMax(1000)).toBe(1000);
    expect(niceBytesMax(4.6 * 1024 ** 3)).toBe(5 * 1024 ** 3);
    expect(niceBytesMax(700)).toBe(1000);
  });

  it("names days and months in the viewer's language", () => {
    expect(formatPeriod("2026-09-27", "days", "en")).toBe("Sep 27");
    expect(formatPeriod("2026-09", "months", "de")).toBe("Sept. 26");
  });
});
