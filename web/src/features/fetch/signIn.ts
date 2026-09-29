import type { FetchAccountInfo, SignInProvider } from "@/lib/api";

/**
 * Microsoft's own address domains and Google's, the way the server knows them
 * (crates/uwumail-smtp/src/provider_oauth.rs): the dialog offers signing in there at once, before
 * the server has even been asked. Microsoft 365 and Google Workspace domains are only found by
 * their mail servers, which the server looks up.
 */
export function providerOfAddress(address: string): SignInProvider | null {
  const at = address.lastIndexOf("@");
  if (at < 0) return null;
  const domain = address
    .slice(at + 1)
    .trim()
    .replace(/\.$/, "")
    .toLowerCase();
  if (domain === "gmail.com" || domain === "googlemail.com") return "google";
  if (domain === "msn.com" || domain === "windowslive.com" || domain === "passport.com") return "microsoft";
  const dot = domain.indexOf(".");
  if (dot < 0) return null;
  const name = domain.slice(0, dot);
  const labels = domain.slice(dot + 1).split(".");
  const suffixOk = labels.length <= 2 && labels.every((label) => /^[a-z]{2,3}$/.test(label));
  return suffixOk && ["hotmail", "live", "outlook"].includes(name) ? "microsoft" : null;
}

/** Whether an address is complete enough to ask the server about its domain's mail servers. */
export function looksLikeAddress(address: string): boolean {
  return /^[^\s@]+@[^\s@]+\.[^\s@]{2,}$/.test(address.trim());
}

/** The codes a sign-in fails with that the page has its own sentence for; anything else is "failed". */
const SIGN_IN_ERRORS = [
  "declined",
  "expired",
  "failed",
  "noRefreshToken",
  "oauthClientRejected",
  "oauthNotConfigured",
  "providerUnreachable",
  "signInRefused",
  "busy",
] as const;

export type SignInError = (typeof SIGN_IN_ERRORS)[number];

export function signInError(code: string | null | undefined): SignInError {
  return (SIGN_IN_ERRORS as readonly string[]).includes(code ?? "") ? (code as SignInError) : "failed";
}

/** What Google's way back left in the address bar: a sign-in to finish, or why there is none. */
export type SignInReturn = { flow: string } | { error: SignInError } | null;

export function readSignInReturn(search: string): SignInReturn {
  const params = new URLSearchParams(search);
  const flow = params.get("oauth");
  if (flow && /^[\w-]{8,64}$/.test(flow)) return { flow };
  if (params.has("oauthError")) return { error: signInError(params.get("oauthError")) };
  return null;
}

/** The address without the sign-in's parameters, for putting back into the address bar. */
export function withoutSignInReturn(search: string): string {
  const params = new URLSearchParams(search);
  params.delete("oauth");
  params.delete("oauthError");
  const rest = params.toString();
  return rest ? `?${rest}` : "";
}

/**
 * What the dialog had chosen before the browser went to Google and came back: kept in the tab's
 * session storage for the way there and back only. A tab without storage still finishes the
 * sign-in, with the defaults.
 */
export interface SignInDraft {
  afterFetch: FetchAccountInfo["afterFetch"];
  fetchJunk: boolean;
  intervalSecs: number;
  takeExisting: boolean;
}

const DRAFT_KEY = "uwumail.fetch.signInDraft";

export function keepDraft(draft: SignInDraft, storage: Pick<Storage, "setItem"> | null = safeStorage()): void {
  try {
    storage?.setItem(DRAFT_KEY, JSON.stringify(draft));
  } catch {
    // Without storage the defaults do.
  }
}

export function takeDraft(storage: Pick<Storage, "getItem" | "removeItem"> | null = safeStorage()): SignInDraft | null {
  try {
    const raw = storage?.getItem(DRAFT_KEY);
    storage?.removeItem(DRAFT_KEY);
    if (!raw) return null;
    const value = JSON.parse(raw) as Partial<SignInDraft>;
    if (value.afterFetch !== "markRead" && value.afterFetch !== "delete") return null;
    if (typeof value.intervalSecs !== "number" || typeof value.fetchJunk !== "boolean") return null;
    return {
      afterFetch: value.afterFetch,
      fetchJunk: value.fetchJunk,
      intervalSecs: value.intervalSecs,
      takeExisting: value.takeExisting === true,
    };
  } catch {
    return null;
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof window === "undefined" ? null : window.sessionStorage;
  } catch {
    return null;
  }
}

/** What a mailbox row has to say about its sign-in, most urgent first. */
export type SignInNeed =
  | { kind: "expired"; provider: SignInProvider }
  | { kind: "passwordRefused" }
  | { kind: "canSwitch"; provider: SignInProvider }
  | null;

export function signInNeed(account: FetchAccountInfo): SignInNeed {
  if (account.auth !== "password" && account.loginExpired) return { kind: "expired", provider: account.auth };
  if (account.auth === "password" && account.passwordRefused) return { kind: "passwordRefused" };
  if (account.auth === "password" && account.signIn) return { kind: "canSwitch", provider: account.signIn };
  return null;
}
