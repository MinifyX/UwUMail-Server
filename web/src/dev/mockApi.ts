/**
 * A pretend server for `pnpm dev --mode mock`: every page with sample data, no real server
 * and no password needed. Add `?loggedOut` to the URL to see the login page; the password
 * page works with any token except "expired".
 * Production builds never include this file.
 */

import type { AuditRecord, DomainSummary, Info, Overview, Person, Profile, Session } from "@/lib/api";

const now = Math.floor(Date.now() / 1000);
const GB = 1024 ** 3;
let loggedIn = !new URLSearchParams(window.location.search).has("loggedOut");
let preferences: Record<string, unknown> = {};

const session = (): Session => ({
  account: { id: 1, login: "lorin@uwu.example", name: "Lorin", role: "admin" },
  csrfToken: "mock",
  preferences,
  server: { hostname: "mail.uwu.example", version: "0.1.0" },
});

const address = (value: string, kind: "primary" | "alias" = "primary") => ({
  address: value,
  kind,
  createdAt: now - 86_400,
});

function person(login: string, name: string, extra: Partial<Person> = {}): Person {
  return {
    login,
    name,
    role: "user",
    status: "active",
    quotaBytes: 5 * GB,
    usedBytes: Math.round(Math.random() * 3 * GB),
    createdAt: now - 12 * 86_400,
    deletedAt: null,
    purgeAt: null,
    addresses: [address(login)],
    ...extra,
  };
}

const people: Person[] = [
  person("lorin@uwu.example", "Lorin", {
    role: "admin",
    quotaBytes: 0,
    addresses: [
      address("lorin@uwu.example"),
      address("hallo@uwu.example", "alias"),
      address("nyu@uwu.example", "alias"),
    ],
  }),
  person("leni@uwu.example", "Leni Wanders", { quotaBytes: 2 * GB, usedBytes: 1.8 * GB }),
  person("ami@uwu.example", "Ami", { status: "invited", usedBytes: 0 }),
  person("opa@verein.example", "Opa Heinz", { status: "disabled" }),
  person("kassenwart@verein.example", "Kassenwart", {
    addresses: [address("kassenwart@verein.example"), address("kasse@verein.example", "alias")],
  }),
  person("alt@uwu.example", "Altes Konto", {
    status: "deleted",
    deletedAt: now - 3 * 86_400,
    purgeAt: now + 27 * 86_400,
  }),
];

const domains: DomainSummary[] = [
  { name: "uwu.example", catchAll: null, createdAt: now - 30 * 86_400 },
  { name: "verein.example", catchAll: null, createdAt: now - 20 * 86_400 },
];

let nextAuditId = 20;
const audit: AuditRecord[] = [
  {
    id: 5,
    at: now - 600,
    actor: "lorin@uwu.example",
    action: "account.update",
    target: "leni@uwu.example",
    details: { quotaBytes: 2 * GB },
    ip: "192.0.2.10",
  },
  {
    id: 4,
    at: now - 3600,
    actor: "lorin@uwu.example",
    action: "account.create",
    target: "ami@uwu.example",
    details: { role: "user", invited: true },
    ip: "192.0.2.10",
  },
  {
    id: 3,
    at: now - 86_400 - 200,
    actor: "leni@uwu.example",
    action: "account.passwordChosen",
    target: "leni@uwu.example",
    details: { purpose: "invite" },
    ip: "198.51.100.7",
  },
  {
    id: 2,
    at: now - 3 * 86_400,
    actor: "lorin@uwu.example",
    action: "account.trash",
    target: "alt@uwu.example",
    details: {},
    ip: "192.0.2.10",
  },
  { id: 1, at: now - 12 * 86_400, actor: "cli", action: "domain.create", target: "uwu.example", details: {}, ip: "" },
];

function log(action: string, target: string, details: Record<string, unknown> = {}) {
  audit.unshift({
    id: nextAuditId++,
    at: Math.floor(Date.now() / 1000),
    actor: "lorin@uwu.example",
    action,
    target,
    details,
    ip: "192.0.2.10",
  });
}

const problem = (status: number, code: string): [number, unknown] => [status, { code, detail: code }];
const link = () => ({ path: `/password/mock-${Math.random().toString(36).slice(2)}`, expiresAt: now + 7 * 86_400 });

type Handler = (body: unknown, params: string[]) => [number, unknown];

const routes: [string, RegExp, Handler][] = [
  ["GET", /^\/api\/info$/, () => [200, { hostname: "mail.uwu.example", setupRequired: false } satisfies Info]],
  ["GET", /^\/api\/session$/, () => [200, loggedIn ? session() : null]],
  [
    "POST",
    /^\/api\/auth\/login$/,
    (body) => {
      if ((body as { login: string }).login.startsWith("wrong")) return problem(401, "invalidCredentials");
      loggedIn = true;
      return [200, session()];
    },
  ],
  [
    "POST",
    /^\/api\/auth\/logout$/,
    () => {
      loggedIn = false;
      return [204, null];
    },
  ],
  [
    "GET",
    /^\/api\/account$/,
    () => [
      200,
      {
        login: "lorin@uwu.example",
        name: "Lorin",
        role: "admin",
        addresses: ["lorin@uwu.example", "hallo@uwu.example", "nyu@uwu.example"],
        quotaBytes: 5 * GB,
        usedBytes: 1.3 * GB,
        createdAt: now - 20 * 86_400,
      } satisfies Profile,
    ],
  ],
  [
    "PATCH",
    /^\/api\/account\/preferences$/,
    (body) => {
      preferences = { ...preferences, ...(body as object) };
      return [200, preferences];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/overview$/,
    () => [
      200,
      {
        counts: {
          domains: domains.length,
          accounts: people.filter((p) => p.status !== "deleted").length,
          admins: 1,
          disabledAccounts: people.filter((p) => p.status === "disabled").length,
          aliases: 3,
          usedBytes: 8.4 * GB,
          queuedMessages: 2,
          pendingRecipients: 3,
          deferredRecipients: 1,
        },
        server: { hostname: "mail.uwu.example", version: "0.1.0", uptimeSeconds: 3 * 86_400 + 5 * 3600 },
      } satisfies Overview,
    ],
  ],
  ["GET", /^\/api\/admin\/domains$/, () => [200, domains]],
  ["GET", /^\/api\/admin\/audit$/, () => [200, audit]],
  ["GET", /^\/api\/admin\/people$/, () => [200, people]],
  [
    "POST",
    /^\/api\/admin\/people$/,
    (body) => {
      const input = body as { address: string; name: string; admin: boolean; quotaBytes: number; password?: string };
      const login = input.address.toLowerCase();
      if (people.some((p) => p.addresses.some((a) => a.address === login))) return problem(409, "conflict");
      const created = person(login, input.name, {
        role: input.admin ? "admin" : "user",
        quotaBytes: input.quotaBytes,
        usedBytes: 0,
        status: input.password ? "active" : "invited",
        createdAt: Math.floor(Date.now() / 1000),
      });
      people.push(created);
      people.sort((a, b) => a.login.localeCompare(b.login));
      log("account.create", login, { invited: !input.password });
      return [201, { person: created, link: input.password ? null : link() }];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/people\/([^/]+)$/,
    (_, [login]) => {
      const found = people.find((p) => p.login === login);
      return found ? [200, found] : problem(404, "notFound");
    },
  ],
  [
    "PATCH",
    /^\/api\/admin\/people\/([^/]+)$/,
    (body, [login]) => {
      const found = people.find((p) => p.login === login);
      if (!found) return problem(404, "notFound");
      const changes = body as { name?: string; admin?: boolean; quotaBytes?: number; disabled?: boolean };
      if (login === "lorin@uwu.example" && (changes.admin === false || changes.disabled)) {
        return problem(409, changes.disabled ? "notYourself" : "lastAdmin");
      }
      if (changes.name !== undefined) found.name = changes.name;
      if (changes.admin !== undefined) found.role = changes.admin ? "admin" : "user";
      if (changes.quotaBytes !== undefined) found.quotaBytes = changes.quotaBytes;
      if (changes.disabled !== undefined) found.status = changes.disabled ? "disabled" : "active";
      log("account.update", login!, changes);
      return [200, found];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/people\/([^/]+)$/,
    (_, [login]) => {
      const found = people.find((p) => p.login === login);
      if (!found) return problem(404, "notFound");
      if (login === "lorin@uwu.example") return problem(409, "notYourself");
      Object.assign(found, { status: "deleted", deletedAt: now, purgeAt: now + 30 * 86_400 });
      log("account.trash", login!);
      return [200, found];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/people\/([^/]+)\/restore$/,
    (_, [login]) => {
      const found = people.find((p) => p.login === login);
      if (!found) return problem(404, "notFound");
      Object.assign(found, { status: "active", deletedAt: null, purgeAt: null });
      log("account.restore", login!);
      return [200, found];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/people\/([^/]+)\/purge$/,
    (body, [login]) => {
      if ((body as { confirm: string }).confirm.toLowerCase() !== login) return problem(409, "confirmationMismatch");
      people.splice(
        people.findIndex((p) => p.login === login),
        1,
      );
      log("account.purge", login!);
      return [204, null];
    },
  ],
  ["POST", /^\/api\/admin\/people\/([^/]+)\/password-link$/, () => [200, link()]],
  [
    "PUT",
    /^\/api\/admin\/people\/([^/]+)\/password$/,
    (body) => ((body as { password: string }).password.length < 10 ? problem(409, "weakPassword") : [204, null]),
  ],
  [
    "POST",
    /^\/api\/admin\/people\/([^/]+)\/aliases$/,
    (body, [login]) => {
      const found = people.find((p) => p.login === login);
      const value = (body as { address: string }).address.toLowerCase();
      if (!found) return problem(404, "notFound");
      if (people.some((p) => p.addresses.some((a) => a.address === value))) return problem(409, "conflict");
      found.addresses.push(address(value, "alias"));
      log("alias.add", value, { account: login });
      return [201, found];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/people\/([^/]+)\/aliases\/([^/]+)$/,
    (_, [login, value]) => {
      const found = people.find((p) => p.login === login);
      if (!found) return problem(404, "notFound");
      found.addresses = found.addresses.filter((a) => a.address !== value);
      log("alias.remove", value!, { account: login });
      return [200, found];
    },
  ],
  [
    "GET",
    /^\/api\/password-links\/([^/]+)$/,
    (_, [token]) =>
      token === "expired"
        ? problem(409, "linkInvalid")
        : [
            200,
            {
              login: "ami@uwu.example",
              name: "Ami",
              purpose: token?.startsWith("reset") ? "reset" : "invite",
              expiresAt: now + 6 * 86_400,
            },
          ],
  ],
  [
    "POST",
    /^\/api\/password-links\/([^/]+)$/,
    (body) => {
      if ((body as { password: string }).password.length < 10) return problem(409, "weakPassword");
      loggedIn = true;
      return [200, session()];
    },
  ],
];

const realFetch = window.fetch.bind(window);

window.fetch = async (input, init) => {
  const url = new URL(
    typeof input === "string" ? input : input instanceof URL ? input.href : input.url,
    window.location.href,
  );
  if (!url.pathname.startsWith("/api/")) return realFetch(input, init);
  const method = (init?.method ?? "GET").toUpperCase();
  const body = typeof init?.body === "string" ? JSON.parse(init.body) : undefined;
  await new Promise((resolve) => setTimeout(resolve, 250));
  let result: [number, unknown] = problem(404, "notFound");
  for (const [routeMethod, pattern, handler] of routes) {
    const match = routeMethod === method ? pattern.exec(url.pathname) : null;
    if (match) {
      result = handler(body, match.slice(1).map(decodeURIComponent));
      break;
    }
  }
  const [status, data] = result;
  return new Response(status === 204 ? null : JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
};
