/** The server's JSON API under /api. */

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    detail: string,
  ) {
    super(detail);
    this.name = "ApiError";
  }
}

let csrfToken: string | null = null;

/** The session's token for requests that change something; set after login and on start. */
export function setCsrfToken(token: string | null) {
  csrfToken = token;
}

export async function api<T>(path: string, options: { method?: string; body?: unknown } = {}): Promise<T> {
  const method = options.method ?? "GET";
  const headers: Record<string, string> = { Accept: "application/json" };
  if (options.body !== undefined) headers["Content-Type"] = "application/json";
  if (method !== "GET" && csrfToken) headers["X-CSRF-Token"] = csrfToken;

  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers,
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
      credentials: "same-origin",
    });
  } catch {
    throw new ApiError(0, "offline", "The server cannot be reached.");
  }
  if (response.status === 204) return undefined as T;
  const data: unknown = await response.json().catch(() => null);
  if (!response.ok) {
    const problem = (data ?? {}) as { code?: string; detail?: string };
    throw new ApiError(response.status, problem.code ?? "internal", problem.detail ?? response.statusText);
  }
  return data as T;
}

export type Role = "admin" | "user";

export interface Info {
  hostname: string;
  setupRequired: boolean;
}

export interface Session {
  account: { id: number; login: string; name: string; role: Role };
  csrfToken: string;
  preferences: Record<string, unknown>;
  server: { hostname: string; version: string };
}

export interface Profile {
  login: string;
  name: string;
  role: Role;
  addresses: string[];
  quotaBytes: number;
  usedBytes: number;
  createdAt: number;
}

export interface Overview {
  counts: {
    domains: number;
    accounts: number;
    admins: number;
    disabledAccounts: number;
    aliases: number;
    usedBytes: number;
    queuedMessages: number;
    pendingRecipients: number;
    deferredRecipients: number;
  };
  server: { hostname: string; version: string; uptimeSeconds: number };
}

export type PersonStatus = "active" | "invited" | "disabled" | "deleted";

export interface AddressInfo {
  address: string;
  kind: "primary" | "alias";
  createdAt: number;
}

export interface Person {
  login: string;
  name: string;
  role: Role;
  status: PersonStatus;
  quotaBytes: number;
  usedBytes: number;
  createdAt: number;
  deletedAt: number | null;
  purgeAt: number | null;
  addresses: AddressInfo[];
}

/** A one-time link to choose a password; `path` is relative to the portal. */
export interface PasswordLinkCreated {
  path: string;
  expiresAt: number;
}

export interface PasswordLinkInfo {
  login: string;
  name: string;
  purpose: "invite" | "reset";
  expiresAt: number;
}

export type CheckStatus = "ok" | "warning" | "missing" | "wrong" | "error";
export type DkimKeyState = "active" | "pending" | "retired";

export interface DomainSummary {
  name: string;
  catchAll: string | null;
  createdAt: number;
  people: number;
  aliases: number;
  dns: { status: CheckStatus; checkedAt: number } | null;
}

export interface RecordCheck {
  kind: "mx" | "spf" | "dmarc" | "dkim";
  name: string;
  recordType: "MX" | "TXT";
  expected: string;
  found: string[];
  status: CheckStatus;
  note: string | null;
  selector: string | null;
  keyState: DkimKeyState | null;
}

export interface DomainReport {
  domain: string;
  checkedAt: number;
  source: "authoritative" | "resolver";
  nameservers: string[];
  status: CheckStatus;
  records: RecordCheck[];
}

export interface DkimKeyInfo {
  selector: string;
  algorithm: "rsa-sha256" | "ed25519-sha256";
  state: DkimKeyState;
  createdAt: number;
  retiredAt: number | null;
  dnsName: string;
  dnsValue: string;
}

export interface DomainDetail extends Omit<DomainSummary, "dns"> {
  keys: DkimKeyInfo[];
  report: DomainReport | null;
  setup: { hostname: string; relayHost: string | null; upstreamMx: boolean };
}

export interface AuditRecord {
  id: number;
  at: number;
  actor: string;
  action: string;
  target: string;
  details: Record<string, unknown>;
  ip: string;
}

/** Turns a link path from the API into a full URL for copying. */
export function absoluteUrl(path: string): string {
  return new URL(path, window.location.origin).href;
}
