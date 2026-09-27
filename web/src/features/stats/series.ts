import type { StatsRange, StatsView } from "@/lib/api";

/** One bar: a day or a month, and its value; `null` where there is no reading at all. */
export interface Point {
  period: string;
  value: number | null;
}

/**
 * What each chart shows: one stored key, or the sum of several (every refusal reason, every protocol).
 * Greylisted mail is not among the refused: most of it comes back and is counted as received then.
 */
export const REFUSED_KEYS = ["refused.unknownRecipient", "refused.spam", "refused.virus", "refused.policy"] as const;
export const LOGIN_KEYS = [
  "loginFailed.smtp",
  "loginFailed.imap",
  "loginFailed.jmap",
  "loginFailed.dav",
  "loginFailed.managesieve",
  "loginFailed.portal",
  "loginFailed.other",
] as const;

/** Counters add up; a period without any is a zero. Gauges (`gauge.*`) are missing where nothing was read. */
export function series(view: StatsView, keys: readonly string[]): Point[] {
  const gauge = keys.every((key) => key.startsWith("gauge."));
  return view.periods.map(({ period, values }) => {
    const present = keys.filter((key) => key in values);
    if (gauge && present.length === 0) return { period, value: null };
    return { period, value: present.reduce((sum, key) => sum + (values[key] ?? 0), 0) };
  });
}

export function total(view: StatsView, keys: readonly string[]): number {
  return keys.reduce((sum, key) => sum + (view.totals[key] ?? 0), 0);
}

/** A round top for the axis: 1, 2 or 5 times a power of ten, at least the largest value. */
export function niceMax(max: number): number {
  if (max <= 0) return 1;
  const power = 10 ** Math.floor(Math.log10(max));
  const step = [1, 2, 5, 10].find((factor) => factor * power >= max) ?? 10;
  return step * power;
}

/** [`niceMax`] in the binary unit the value is shown in, so a byte axis ends at 5 GB rather than 4.7 GB. */
export function niceBytesMax(max: number): number {
  let unit = 1;
  while (max / unit >= 1024) unit *= 1024;
  return niceMax(max / unit) * unit;
}

/** `2026-09-27` or `2026-09` as the viewer reads dates. Stats are kept per UTC day. */
export function formatPeriod(period: string, range: StatsRange, language: string, long = false): string {
  const [year, month, day] = period.split("-").map(Number);
  const date = new Date(Date.UTC(year ?? 1970, (month ?? 1) - 1, day ?? 1));
  const options: Intl.DateTimeFormatOptions =
    range === "days"
      ? { day: "numeric", month: long ? "long" : "short", timeZone: "UTC", ...(long ? { weekday: "short" } : {}) }
      : { month: long ? "long" : "short", year: long ? "numeric" : "2-digit", timeZone: "UTC" };
  return new Intl.DateTimeFormat(language, options).format(date);
}
