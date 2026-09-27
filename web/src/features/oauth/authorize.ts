/**
 * The consent page of the OAuth provider (docs/oauth.md). An app sends the browser to
 * `/oauth/authorize?…`; the page passes that request on to the server unchanged, asks the person,
 * and the server's answer sends the browser back to the app.
 */

/** The answer to the server: every parameter of the app's request as it came, and the decision. */
export function decisionBody(search: string, approve: boolean): Record<string, string | boolean> {
  return { ...Object.fromEntries(new URLSearchParams(search)), approve };
}

/** Scopes the page explains, in the order it lists them; anything else is left out. */
export const KNOWN_SCOPES = ["mail", "smtp", "dav", "openid", "profile", "email", "offline_access"] as const;

export type KnownScope = (typeof KNOWN_SCOPES)[number];

export function knownScopes(scopes: string[]): KnownScope[] {
  return KNOWN_SCOPES.filter((scope) => scopes.includes(scope));
}
