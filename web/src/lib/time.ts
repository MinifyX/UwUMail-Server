/**
 * Times of day that the server keeps in UTC and the browser shows where you are.
 *
 * Backups and updates both do this, and they have to agree: the whole point of the half hour the
 * update keeps away from the backup is that the two are measured against the same clock. So the
 * arithmetic lives here once instead of twice.
 *
 * Whole minutes, not whole hours — half-hour zones like India's exist. Daylight saving is taken as
 * it stands today, so a time chosen in summer moves by an hour in winter, the same as everywhere
 * else that keeps a UTC hour.
 */

export const offsetMinutes = () => -new Date().getTimezoneOffset();

export const wrapDay = (minutes: number) => ((minutes % 1440) + 1440) % 1440;
const wrapWeek = (day: number) => ((day % 7) + 7) % 7;

/** A UTC time of day as minutes since local midnight. */
export const localTime = (hour: number, minute: number) => wrapDay(hour * 60 + minute + offsetMinutes());

/** And back. */
export const utcTime = (local: number) => wrapDay(local - offsetMinutes());

/** Five-minute steps, plus whatever minute is set now, so a time from the command line survives. */
export const minuteOptions = (current: number) =>
  [...new Set([...Array.from({ length: 12 }, (_, step) => step * 5), current])].sort((a, b) => a - b);

/**
 * A weekly time, where the day moves with the hour: Monday 23:00 UTC is Tuesday in Berlin, and a
 * schedule that forgot that would run a day off for half the world.
 *
 * `weekday` is 0 for Monday, or null for every day.
 */
export function localWeekly(weekday: number | null, hour: number, minute: number) {
  const raw = hour * 60 + minute + offsetMinutes();
  return { weekday: weekday === null ? null : wrapWeek(weekday + Math.floor(raw / 1440)), time: wrapDay(raw) };
}

export function utcWeekly(weekday: number | null, time: number) {
  const raw = time - offsetMinutes();
  return {
    weekday: weekday === null ? null : wrapWeek(weekday + Math.floor(raw / 1440)),
    hour: Math.floor(wrapDay(raw) / 60),
    minute: wrapDay(raw) % 60,
  };
}

/** Monday first, in the reader's language. */
export function weekdayNames(language: string): string[] {
  const format = new Intl.DateTimeFormat(language, { weekday: "long", timeZone: "UTC" });
  // The 1st of January 2024 was a Monday.
  return Array.from({ length: 7 }, (_, day) => format.format(new Date(Date.UTC(2024, 0, 1 + day))));
}

/** "04:30" from minutes since midnight. */
export const clock = (minutes: number) =>
  `${String(Math.floor(wrapDay(minutes) / 60)).padStart(2, "0")}:${String(wrapDay(minutes) % 60).padStart(2, "0")}`;
