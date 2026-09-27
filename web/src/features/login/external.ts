/**
 * Logging in at another provider with OpenID Connect (docs/login-oidc-ldap.md). The provider's
 * answer goes to the server, which sends the browser back to the login page: with an error code,
 * or, for an account with a second factor, with a pending login that the second step finishes.
 */

import type { SecondFactorChallenge } from "@/lib/api";
import { safeNext } from "@/features/session/afterLogin";

export const OIDC_ERRORS = [
  "unavailable",
  "expired",
  "refused",
  "failed",
  "emailNotVerified",
  "domainNotAllowed",
  "noAccount",
  "alreadyLinked",
] as const;

export type OidcError = (typeof OIDC_ERRORS)[number];

/** Why the provider login did not work, from `?oidcError=`; an unknown code counts as `failed`. */
export function oidcError(search: string): OidcError | null {
  const code = new URLSearchParams(search).get("oidcError");
  if (code === null) return null;
  return (OIDC_ERRORS as readonly string[]).includes(code) ? (code as OidcError) : "failed";
}

/** The second step of a provider login, from `?pending=<token>&methods=totp,passkey,recovery`. */
export function pendingLogin(search: string): SecondFactorChallenge | null {
  const params = new URLSearchParams(search);
  const token = params.get("pending");
  if (!token) return null;
  const methods = new Set(
    (params.get("methods") ?? "")
      .split(",")
      .map((method) => method.trim())
      .filter(Boolean),
  );
  return {
    token,
    totp: methods.has("totp"),
    passkey: methods.has("passkey"),
    recoveryCodes: methods.has("recovery"),
  };
}

/**
 * The address without what the server handed over, so a reload or a bookmark does not repeat it.
 * Only `next` stays: it still says where to go after the login.
 */
export function withoutHandover(path: string, search: string): string {
  const next = new URLSearchParams(search).get("next");
  return next === null ? path : `${path}?${new URLSearchParams({ next }).toString()}`;
}

/**
 * Where to go after logging in, as the login page shown at `path` sees it: `?next=` on the login
 * page itself, or the page that asked for a login (such as an app's consent page) everywhere else.
 */
export function loginNext(path: string, search: string): string | null {
  if (path === "/" || path === "/login") return safeNext(new URLSearchParams(search).get("next"));
  return safeNext(path + search);
}

/** The address that starts a login at the provider; the server comes back to `next` afterwards. */
export function oidcStartUrl(next: string | null): string {
  return next ? `/api/auth/oidc/start?${new URLSearchParams({ next }).toString()}` : "/api/auth/oidc/start";
}
