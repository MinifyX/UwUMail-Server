import type { TFunction } from "i18next";

const UNITS = ["byte", "kilobyte", "megabyte", "gigabyte", "terabyte"] as const;

export function formatBytes(bytes: number, language: string): string {
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return new Intl.NumberFormat(language, {
    style: "unit",
    unit: UNITS[unit],
    unitDisplay: "short",
    maximumFractionDigits: unit === 0 || value >= 100 ? 0 : 1,
  }).format(value);
}

/** "3 days, 4 hours" — the two largest parts are enough to tell how long something has been running. */
export function formatDuration(seconds: number, t: TFunction): string {
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const parts: string[] = [];
  if (days > 0) parts.push(t("duration.days", { count: days }));
  if (hours > 0) parts.push(t("duration.hours", { count: hours }));
  if (days === 0) parts.push(t("duration.minutes", { count: minutes }));
  return parts.slice(0, 2).join(", ");
}

export function formatDate(unixSeconds: number, language: string): string {
  return new Intl.DateTimeFormat(language, { dateStyle: "long" }).format(new Date(unixSeconds * 1000));
}

export function formatDateTime(unixSeconds: number, language: string): string {
  return new Intl.DateTimeFormat(language, { dateStyle: "medium", timeStyle: "short" }).format(
    new Date(unixSeconds * 1000),
  );
}

export function formatTime(unixSeconds: number, language: string): string {
  return new Intl.DateTimeFormat(language, { timeStyle: "short" }).format(new Date(unixSeconds * 1000));
}

/** Midnight of the day, in local time, as a key to group entries by day. */
export function dayKey(unixSeconds: number): number {
  const date = new Date(unixSeconds * 1000);
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}
