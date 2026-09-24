/**
 * The spam filter's rules and the VPN for the pretend server: a few hundred rules over several
 * domains and people, so the table's search, filters and pages have something to do.
 */

import { guessSenderKind } from "@/features/spam/senders";
import type {
  EgressView,
  NewRule,
  Rule,
  RuleChange,
  RuleKind,
  RuleListName,
  RuleScope,
  RulesView,
  VpnChange,
  VpnConfig,
  VpnView,
} from "@/lib/api";

type Handler = (body: unknown, params: string[], search: URLSearchParams) => [number, unknown];

const now = Math.floor(Date.now() / 1000);
const DAY = 86_400;
const problem = (status: number, code: string, detail = code): [number, unknown] => [status, { code, detail }];

const DOMAINS = ["uwu.example", "verein.example", "praxis-sonnenschein.example", "kita-regenbogen.example"];
const PEOPLE = ["mini@uwu.example", "leni@uwu.example", "ami@verein.example", "doc@praxis-sonnenschein.example"];

const scopes: RuleScope[] = [
  { type: "server" },
  ...DOMAINS.map((name, index) => ({ type: "domain" as const, id: index + 1, name })),
  ...PEOPLE.map((name, index) => ({ type: "account" as const, id: index + 1, name })),
];
const scopeKey = (scope: RuleScope) => (scope.type === "server" ? "server" : `${scope.type}:${scope.name}`);

const SPAMMY = ["casino", "lottery", "viagra", "bitcoin", "crypto", "sweepstake", "winner", "prize", "loan", "seo"];
const TLDS = ["xyz", "top", "click", "loan", "work", "ru", "cn"];

let nextId = 1;
const rules: Rule[] = [];
function add(rule: Omit<Rule, "id" | "createdAt" | "createdBy"> & Partial<Pick<Rule, "createdAt" | "createdBy">>) {
  rules.push({ id: nextId++, createdAt: now - 40 * DAY, createdBy: "mini@uwu.example", ...rule });
}

// A server that has been running for a while: lots of blocked senders, some allowances, some words.
for (let n = 0; n < 180; n++) {
  const scope = scopes[n % 7 === 0 ? 1 + (n % 4) : n % 11 === 0 ? 5 + (n % 4) : 0]!;
  const word = SPAMMY[n % SPAMMY.length]!;
  const tld = TLDS[n % TLDS.length]!;
  const value =
    n % 5 === 0 ? `*.${word}-${n}.${tld}` : n % 3 === 0 ? `${word}${n}.${tld}` : `${word}.${n}@${word}-mail.${tld}`;
  const hits = n % 4 === 0 ? 0 : (n * 37) % 400;
  add({
    type: "sender",
    list: "block",
    kind: guessSenderKind(value) as RuleKind,
    value,
    note: n % 9 === 0 ? "Aus dem Spam-Ordner gemeldet" : "",
    points: null,
    scope,
    expiresAt: n % 13 === 0 ? now + ((n % 60) + 1) * DAY : null,
    hits,
    lastHitAt: hits ? now - ((n * 7919) % (120 * DAY)) : null,
    createdAt: now - ((n * 104_729) % (300 * DAY)),
  });
}
for (const [index, net] of ["198.51.100.0/24", "203.0.113.0/24", "192.0.2.77", "2001:db8:bad::/48"].entries()) {
  add({
    type: "sender",
    list: "block",
    kind: "ip",
    value: net,
    note: "Nur Spam von dort",
    points: null,
    scope: scopes[0]!,
    expiresAt: null,
    hits: 40 * (index + 1),
    lastHitAt: now - 3600 * (index + 1),
  });
}
for (const [index, value] of [
  "newsletter@verein.example",
  "*.mail.partner.example",
  "rechnung@stadtwerke.example",
  "oma@example.net",
  "noreply@kita-app.example",
  "praxis.example",
].entries()) {
  add({
    type: "sender",
    list: "allow",
    kind: guessSenderKind(value) as RuleKind,
    value,
    note: index === 1 ? "Partner" : "",
    points: null,
    scope: scopes[index % 3 === 0 ? 0 : index % 3 === 1 ? 2 : 6]!,
    expiresAt: null,
    hits: (index * 53) % 90,
    lastHitAt: now - index * DAY,
  });
}
for (const [index, value] of [
  "casino",
  "lottery winner",
  "/\\bgewinn(spiel)?\\b/i",
  "krypto",
  "sonderangebot",
  "/\\$\\d{3,}/",
].entries()) {
  add({
    type: "word",
    list: "points",
    kind: value.startsWith("/") ? "regex" : "word",
    value,
    note: "",
    points: index % 2 ? 4 : null,
    scope: scopes[index === 4 ? 6 : 0]!,
    expiresAt: null,
    hits: (index * 29) % 70,
    lastHitAt: index ? now - index * 3 * DAY : null,
  });
}

function matches(rule: Rule, search: URLSearchParams): boolean {
  const q = (search.get("search") ?? "").trim().toLowerCase();
  if (
    q &&
    ![rule.value, rule.note, rule.scope.type === "server" ? "" : rule.scope.name].some((text) =>
      text.toLowerCase().includes(q),
    )
  ) {
    return false;
  }
  const scope = search.get("scope") ?? "all";
  if (scope === "server" && rule.scope.type !== "server") return false;
  if (scope === "domains" && rule.scope.type !== "domain") return false;
  if (scope === "accounts" && rule.scope.type !== "account") return false;
  if ((scope.startsWith("domain:") || scope.startsWith("account:")) && scopeKey(rule.scope) !== scope) return false;
  const state = search.get("state");
  if (state === "temporary" && rule.expiresAt === null) return false;
  if (state === "unused" && rule.hits > 0) return false;
  if (state === "stale" && rule.lastHitAt !== null && rule.lastHitAt > now - 90 * DAY) return false;
  return true;
}

function list(search: URLSearchParams, own: boolean): RulesView {
  const lists = (search.get("list") ?? "").split(",").filter(Boolean) as RuleListName[];
  const kinds = (search.get("kind") ?? "").split(",").filter(Boolean);
  const base = rules.filter(
    (rule) => (own ? scopeKey(rule.scope) === "account:mini@uwu.example" : true) && matches(rule, search),
  );
  const count = <K extends string>(pick: (rule: Rule) => K, pool: Rule[]) =>
    pool.reduce<Partial<Record<K, number>>>(
      (counts, rule) => ({ ...counts, [pick(rule)]: (counts[pick(rule)] ?? 0) + 1 }),
      {},
    );
  const byKind = base.filter((rule) => !kinds.length || kinds.includes(rule.kind));
  const byList = base.filter((rule) => !lists.length || lists.includes(rule.list));
  const found = byKind.filter((rule) => !lists.length || lists.includes(rule.list));
  const sort = search.get("sort") ?? "value";
  const desc = search.get("desc") === "true";
  const key = (rule: Rule): string | number =>
    sort === "hits"
      ? rule.hits
      : sort === "created"
        ? rule.createdAt
        : sort === "lastHit"
          ? (rule.lastHitAt ?? 0)
          : sort === "expires"
            ? (rule.expiresAt ?? Infinity)
            : rule.value.toLowerCase();
  found.sort((a, b) => (key(a) < key(b) ? -1 : key(a) > key(b) ? 1 : a.id - b.id) * (desc ? -1 : 1));
  const perPage = Number(search.get("perPage") ?? 25);
  const page = Number(search.get("page") ?? 0);
  return {
    rules: found.slice(page * perPage, (page + 1) * perPage),
    total: found.length,
    lists: count((rule) => rule.list, byKind),
    kinds: count((rule) => rule.kind, byList),
    page,
    perPage,
    pageSizes: [25, 50, 100, 250],
    defaultPoints: 2.5,
    maxPoints: 10,
  };
}

function scopeOf(key: string | undefined): RuleScope | undefined {
  return scopes.find((scope) => scopeKey(scope) === (key ?? "server"));
}

function create(body: unknown, own: boolean): [number, unknown] {
  const rule = body as NewRule;
  const value = rule.value.trim();
  const scope = own ? scopes.find((item) => scopeKey(item) === "account:mini@uwu.example") : scopeOf(rule.scope);
  if (!scope) return problem(404, "notFound");
  if (rule.type === "sender" && !value.includes(".") && !value.includes(":")) {
    return problem(409, "senderInvalid", `'${value}' needs at least one dot`);
  }
  if (rules.some((item) => item.value === value && scopeKey(item.scope) === scopeKey(scope))) {
    return problem(409, "senderListed", `${value} is already on a list`);
  }
  const created: Rule = {
    id: nextId++,
    type: rule.type,
    list: rule.type === "word" ? "points" : (rule.list ?? "block"),
    kind:
      rule.type === "word"
        ? value.startsWith("/")
          ? "regex"
          : "word"
        : ((rule.kind ?? guessSenderKind(value)) as RuleKind),
    value,
    note: rule.note ?? "",
    points: rule.type === "word" ? (rule.points ?? null) : null,
    scope,
    expiresAt: rule.expiresAt ?? null,
    hits: 0,
    lastHitAt: null,
    createdAt: now,
    createdBy: "mini@uwu.example",
  };
  rules.push(created);
  return [201, created];
}

function change(body: unknown, [type, id]: string[]): [number, unknown] {
  const rule = rules.find((item) => item.type === type && String(item.id) === id);
  if (!rule) return problem(404, "notFound");
  const next = body as RuleChange;
  if (next.value !== undefined) {
    rule.value = next.value.trim();
    if (rule.type === "sender") rule.kind = (next.kind ?? guessSenderKind(rule.value)) as RuleKind;
  }
  if (next.kind) rule.kind = next.kind;
  if (next.list) rule.list = next.list;
  if (next.note !== undefined) rule.note = next.note;
  if (next.points !== undefined) rule.points = next.points;
  if (next.expiresAt !== undefined) rule.expiresAt = next.expiresAt;
  if (next.scope) rule.scope = scopeOf(next.scope) ?? rule.scope;
  return [200, rule];
}

function bulk(body: unknown): [number, unknown] {
  const { items, action, scope, expiresAt } = body as {
    items: { type: string; id: number }[];
    action: string;
    scope?: string;
    expiresAt?: number | null;
  };
  let changed = 0;
  const skipped: { line: string; reason: string }[] = [];
  for (const item of items) {
    const index = rules.findIndex((rule) => rule.type === item.type && rule.id === item.id);
    const rule = rules[index];
    if (!rule) continue;
    if (action === "delete") rules.splice(index, 1);
    else if (action === "allow" || action === "block") {
      if (rule.type === "word") {
        skipped.push({ line: String(rule.id), reason: "word rules add points; they are not allowed or blocked" });
        continue;
      }
      rule.list = action;
    } else if (action === "scope") rule.scope = scopeOf(scope) ?? rule.scope;
    else if (action === "expiry") rule.expiresAt = expiresAt ?? null;
    changed++;
  }
  return [200, { changed, skipped, skippedCount: skipped.length }];
}

function importRules(body: unknown, own: boolean): [number, unknown] {
  const request = body as {
    type: "sender" | "word";
    list: "allow" | "block";
    text: string;
    scope?: string;
    note?: string;
    expiresAt?: number | null;
  };
  let added = 0;
  let duplicates = 0;
  const refused: { line: string; reason: string }[] = [];
  for (const line of request.text.split("\n").map((item) => item.trim())) {
    if (!line || line.startsWith("#")) continue;
    const [value = "", second, ...rest] = line.split(",").map((item) => item.trim());
    const list = second === "allow" || second === "block" ? second : request.list;
    const [status] = create(
      {
        type: request.type,
        list,
        value,
        note: rest.join(",") || request.note,
        scope: request.scope,
        expiresAt: request.expiresAt,
      },
      own,
    );
    if (status === 201) added++;
    else if (status === 409 && rules.some((rule) => rule.value === value)) duplicates++;
    else refused.push({ line: value, reason: "not a sender" });
  }
  return [200, { added, duplicates, refused: refused.slice(0, 20), refusedCount: refused.length }];
}

// The VPN: NordVPN saved, the helper new enough, the container up.
let vpnConfig: VpnConfig = {
  provider: "nordvpn",
  kind: "wireguard",
  countries: "Switzerland",
  regions: "",
  cities: "",
  hostnames: "",
  wireguardPrivateKey: "",
  wireguardPresharedKey: "",
  wireguardAddresses: "",
  wireguardPublicKey: "",
  wireguardEndpointIp: "",
  wireguardEndpointPort: null,
  openvpnUser: "",
  openvpnPassword: "",
  openvpnConfig: "",
};
const vpnSecrets = {
  wireguardPrivateKey: true,
  wireguardPresharedKey: false,
  openvpnPassword: false,
  openvpnConfig: false,
};
let vpnState: "running" | "missing" = "running";
let proxy: string | null = "http://gluetun:8888";
let job: VpnView["job"] = { id: "18c0ffee", state: "done", error: "", at: now - 3 * DAY };
const PROVIDERS = [
  ["nordvpn", "NordVPN", true, false],
  ["mullvad", "Mullvad", true, true],
  ["protonvpn", "Proton VPN", true, false],
  ["surfshark", "Surfshark", true, true],
  ["ivpn", "IVPN", true, true],
  ["airvpn", "AirVPN", true, true],
  ["windscribe", "Windscribe", true, true],
  ["private internet access", "Private Internet Access", false, false],
  ["expressvpn", "ExpressVPN", false, false],
  ["cyberghost", "CyberGhost", false, false],
  ["custom", "Custom", true, true],
] as const;

function vpnView(): VpnView {
  return {
    config: vpnConfig,
    secrets: vpnSecrets,
    saved: true,
    complete: null,
    providers: PROVIDERS.map(([id, name, wireguard, needsAddresses]) => ({
      id,
      name,
      wireguard,
      openvpn: true,
      needsAddresses,
    })),
    helper: {
      available: true,
      canVpn: true,
      version: "2",
      vpn: {
        configured: true,
        provider: vpnConfig.provider,
        type: vpnConfig.kind,
        state: vpnState,
        health: vpnState === "running" ? "healthy" : "",
        always: vpnState === "running",
      },
    },
    job,
    log: "== docker compose --profile vpn up -d gluetun\n Container uwumail-vpn  Recreated\n Container uwumail-vpn  Started\n\n== the VPN is up\n",
    proxy: { current: proxy, gluetun: "http://gluetun:8888", locked: false },
  };
}

export function egressView(): EgressView {
  return {
    proxy,
    fallback: "block",
    fetched: 1284,
    failed: 17,
    proxyFailures: 3,
    fallbacks: 0,
    lastProxyFailure: { at: now - 5 * 3600, error: "the proxy did not answer in time" },
    routes: { pictures: true, updates: true, fetch: false },
  };
}

export const ruleRoutes: [string, RegExp, Handler][] = [
  ["GET", /^\/api\/admin\/spam\/rules$/, (_, __, search) => [200, list(search, false)]],
  ["GET", /^\/api\/account\/spam\/rules$/, (_, __, search) => [200, list(search, true)]],
  ["POST", /^\/api\/admin\/spam\/rules$/, (body) => create(body, false)],
  ["POST", /^\/api\/account\/spam\/rules$/, (body) => create(body, true)],
  ["PATCH", /^\/api\/(?:admin|account)\/spam\/rules\/(sender|word)\/(\d+)$/, (body, params) => change(body, params)],
  ["POST", /^\/api\/(?:admin|account)\/spam\/rules\/bulk$/, (body) => bulk(body)],
  ["POST", /^\/api\/admin\/spam\/rules\/import$/, (body) => importRules(body, false)],
  ["POST", /^\/api\/account\/spam\/rules\/import$/, (body) => importRules(body, true)],
  [
    "GET",
    /^\/api\/admin\/spam\/scopes$/,
    (_, __, search) => {
      const q = (search.get("search") ?? "").toLowerCase();
      return [
        200,
        {
          scopes: scopes
            .filter((scope) => scope.type === "server" || scope.name.includes(q))
            .map((scope) => ({
              key: scopeKey(scope),
              scope,
              count: rules.filter((rule) => scopeKey(rule.scope) === scopeKey(scope)).length,
            })),
        },
      ];
    },
  ],
  ["GET", /^\/api\/admin\/vpn$/, () => [200, vpnView()]],
  [
    "PUT",
    /^\/api\/admin\/vpn$/,
    (body) => {
      const change = body as VpnChange;
      vpnConfig = {
        ...vpnConfig,
        ...change,
        wireguardPrivateKey: "",
        wireguardPresharedKey: "",
        openvpnPassword: "",
        openvpnConfig: "",
      };
      if (change.wireguardPrivateKey) vpnSecrets.wireguardPrivateKey = true;
      return [200, vpnView()];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/vpn\/apply$/,
    () => {
      vpnState = "running";
      proxy = "http://gluetun:8888";
      job = { id: `j${Date.now()}`, state: "done", error: "", at: Math.floor(Date.now() / 1000) };
      return [200, vpnView()];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/vpn\/stop$/,
    () => {
      vpnState = "missing";
      proxy = null;
      return [200, vpnView()];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/vpn\/files$/,
    () => [
      200,
      {
        envFile:
          "VPN_SERVICE_PROVIDER='nordvpn'\nVPN_TYPE='wireguard'\nWIREGUARD_PRIVATE_KEY='…'\nSERVER_COUNTRIES='Switzerland'\n",
        ovpn: null,
      },
    ],
  ],
  ["GET", /^\/api\/admin\/egress$/, () => [200, egressView()]],
];
