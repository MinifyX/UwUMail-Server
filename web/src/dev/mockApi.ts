/**
 * A pretend server for `pnpm dev --mode mock`: every page with sample data, no real server
 * and no password needed. Add `?loggedOut` to the URL to see the login page.
 * Production builds never include this file.
 */

import type { Info, Overview, Profile, Session } from "@/lib/api";

const now = Math.floor(Date.now() / 1000);
let loggedIn = !new URLSearchParams(window.location.search).has("loggedOut");
let preferences: Record<string, unknown> = {};

const session = (): Session => ({
  account: { id: 1, login: "lorin@uwu.example", name: "Lorin", role: "admin" },
  csrfToken: "mock",
  preferences,
  server: { hostname: "mail.uwu.example", version: "0.1.0" },
});

const routes: Record<string, (body: unknown) => [number, unknown]> = {
  "GET /api/info": () => [200, { hostname: "mail.uwu.example", setupRequired: false } satisfies Info],
  "GET /api/session": () => (loggedIn ? [200, session()] : [401, { code: "notLoggedIn", detail: "" }]),
  "POST /api/auth/login": (body) => {
    const { login } = body as { login: string };
    if (login.startsWith("wrong")) return [401, { code: "invalidCredentials", detail: "" }];
    loggedIn = true;
    return [200, session()];
  },
  "POST /api/auth/logout": () => {
    loggedIn = false;
    return [204, null];
  },
  "GET /api/account": () => [
    200,
    {
      login: "lorin@uwu.example",
      name: "Lorin",
      role: "admin",
      addresses: ["lorin@uwu.example", "hallo@uwu.example", "nyu@uwu.example"],
      quotaBytes: 5 * 1024 ** 3,
      usedBytes: 1.3 * 1024 ** 3,
      createdAt: now - 20 * 86_400,
    } satisfies Profile,
  ],
  "PATCH /api/account/preferences": (body) => {
    preferences = { ...preferences, ...(body as object) };
    return [200, preferences];
  },
  "GET /api/admin/overview": () => [
    200,
    {
      counts: {
        domains: 2,
        accounts: 7,
        admins: 1,
        disabledAccounts: 1,
        aliases: 12,
        usedBytes: 8.4 * 1024 ** 3,
        queuedMessages: 2,
        pendingRecipients: 3,
        deferredRecipients: 1,
      },
      server: { hostname: "mail.uwu.example", version: "0.1.0", uptimeSeconds: 3 * 86_400 + 5 * 3600 },
    } satisfies Overview,
  ],
};

const realFetch = window.fetch.bind(window);

window.fetch = async (input, init) => {
  const url = new URL(
    typeof input === "string" ? input : input instanceof URL ? input.href : input.url,
    window.location.href,
  );
  if (!url.pathname.startsWith("/api/")) return realFetch(input, init);
  const method = (init?.method ?? "GET").toUpperCase();
  const handler = routes[`${method} ${url.pathname}`];
  const body = typeof init?.body === "string" ? JSON.parse(init.body) : undefined;
  await new Promise((resolve) => setTimeout(resolve, 250));
  const [status, data] = handler ? handler(body) : [404, { code: "notFound", detail: url.pathname }];
  return new Response(status === 204 ? null : JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
};
