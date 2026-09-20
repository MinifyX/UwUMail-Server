/**
 * Where someone lands after signing in.
 *
 * With a mailbox in the browser that is the mailbox: most people come here to read mail, not to
 * change settings. A `?next=` on the login page wins, but only when it points at this server —
 * otherwise a link into the portal could send someone somewhere else right after they typed
 * their password.
 */

/** A `next` value, if it is a path on this server and nothing else. */
export function safeNext(next: string | null | undefined): string | null {
  if (!next) return null;
  const value = next.trim();
  // A path, never a host: "//elsewhere.example" and "https://elsewhere.example" are both refused,
  // and so is anything a browser might read as one after unescaping.
  if (!value.startsWith("/") || value.startsWith("//") || value.startsWith("/\\")) return null;
  if (value.length > 512) return null;
  // No backslashes, and no control characters: a newline in a redirect is how header splitting
  // starts, and a backslash is how some browsers read a path as a host.
  if (value.includes("\\")) return null;
  if ([...value].some((character) => character.charCodeAt(0) < 0x20 || character.charCodeAt(0) === 0x7f)) return null;
  return value;
}

/** The path to open after signing in: `?next=` if it is safe, else the mailbox, else the account. */
export function afterLogin(search: string, hasWebmail: boolean): string {
  const asked = safeNext(new URLSearchParams(search).get("next"));
  if (asked) return asked;
  return hasWebmail ? "/mail" : "/account";
}

/** Whether a path belongs to the portal's own router or has to be opened as a page. */
export function isPortalPath(path: string): boolean {
  return !path.startsWith("/mail");
}
