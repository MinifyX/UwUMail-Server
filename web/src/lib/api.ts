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
