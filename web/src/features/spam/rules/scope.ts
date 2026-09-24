import type { TFunction } from "i18next";
import type { RuleScope } from "@/lib/api";

/** How the API names a scope: server, domain:<name> or account:<login>. */
export function scopeKey(scope: RuleScope): string {
  return scope.type === "server" ? "server" : `${scope.type}:${scope.name}`;
}

/** A scope filter or target as people read it. */
export function scopeText(key: string, t: TFunction): string {
  if (key === "" || key === "all") return t("spam.rules.scope.all");
  if (key === "server") return t("spam.rules.scope.server");
  if (key === "domains") return t("spam.rules.scope.domains");
  if (key === "accounts") return t("spam.rules.scope.accounts");
  const [type, ...rest] = key.split(":");
  const name = rest.join(":");
  return type === "domain" ? t("spam.rules.scope.domain", { name }) : t("spam.rules.scope.account", { name });
}

export function scopeLabel(scope: RuleScope, t: TFunction): string {
  return scope.type === "server" ? t("spam.rules.scope.server") : scope.name;
}

const DAY = 86_400;

/** The end dates offered for a rule, in days from now; 0 means for good. */
export const EXPIRY_PRESETS = [0, 1, 7, 30, 90, 365] as const;

export function expiryFromDays(days: number): number | null {
  return days === 0 ? null : Math.floor(Date.now() / 1000) + days * DAY;
}

/** A date input's value (YYYY-MM-DD) as the end of that day, local time. */
export function expiryFromDate(value: string): number | null {
  if (!value) return null;
  const [year, month, day] = value.split("-").map(Number);
  if (!year || !month || !day) return null;
  return Math.floor(new Date(year, month - 1, day, 23, 59, 59).getTime() / 1000);
}

export function dateInputValue(unixSeconds: number | null): string {
  if (unixSeconds === null) return "";
  const date = new Date(unixSeconds * 1000);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}
