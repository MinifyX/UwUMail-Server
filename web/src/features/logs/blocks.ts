import type { LogCategory } from "@/lib/api";

/** The kinds of log line, in the order their chips are shown. */
export const LOG_CATEGORIES: LogCategory[] = [
  "login",
  "login_failed",
  "block",
  "mail_in",
  "mail_out",
  "gateway",
  "system",
];

/** Switches one kind on or off; the result keeps the order of the chips whatever was clicked first. */
export function toggleCategory(current: LogCategory[], category: LogCategory): LogCategory[] {
  return LOG_CATEGORIES.filter((value) => (value === category) !== current.includes(value));
}

function isV4(address: string): boolean {
  const parts = address.split(".");
  return parts.length === 4 && parts.every((part) => /^\d{1,3}$/.test(part) && Number(part) <= 255);
}

function isV6(address: string): boolean {
  if (!/^[0-9a-f:]+$/i.test(address) || address.includes(":::")) return false;
  const halves = address.split("::");
  if (halves.length > 2) return false;
  const groups = halves.flatMap((half) => (half === "" ? [] : half.split(":")));
  if (groups.some((group) => group.length === 0 || group.length > 4)) return false;
  return halves.length === 2 ? groups.length < 8 : groups.length === 8;
}

/**
 * Whether a value could be an IP address or a network in CIDR form, v4 or v6. Only a sanity check
 * before saving: the server parses the value and has the last word.
 */
export function looksLikeNetwork(value: string): boolean {
  const [address = "", prefix, ...rest] = value.trim().split("/");
  if (rest.length > 0) return false;
  const v4 = isV4(address);
  if (!v4 && !isV6(address)) return false;
  return prefix === undefined || (/^\d{1,3}$/.test(prefix) && Number(prefix) <= (v4 ? 32 : 128));
}
