/**
 * A pretend server for `pnpm dev --mode mock`: every page with sample data, no real server
 * and no password needed. Add `?loggedOut` to the URL to see the login page; the password
 * page works with any token except "expired".
 * Production builds never include this file.
 */

import type {
  AppPasswordInfo,
  AuditRecord,
  Health,
  HealthArea,
  HealthFinding,
  DkimKeyInfo,
  DomainDetail,
  DomainReport,
  DomainSummary,
  Info,
  Overview,
  Person,
  Profile,
  RecordCheck,
  SecurityView,
  Session,
} from "@/lib/api";

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

interface MockDomain {
  name: string;
  catchAll: string | null;
  createdAt: number;
  keys: DkimKeyInfo[];
  report: DomainReport | null;
}

const key = (domain: string, selector: string, state: DkimKeyInfo["state"], algorithm: DkimKeyInfo["algorithm"]) => ({
  selector,
  algorithm,
  state,
  createdAt: now - 30 * 86_400,
  retiredAt: state === "retired" ? now - 86_400 : null,
  dnsName: `${selector}._domainkey.${domain}`,
  dnsValue: `v=DKIM1; k=${algorithm === "rsa-sha256" ? "rsa" : "ed25519"}; p=MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAr${selector}`,
});

function record(
  kind: RecordCheck["kind"],
  name: string,
  expected: string,
  found: string[],
  extra: Partial<RecordCheck> = {},
): RecordCheck {
  return {
    kind,
    name,
    recordType: kind === "mx" ? "MX" : "TXT",
    expected,
    found,
    status: "ok",
    note: null,
    selector: null,
    keyState: null,
    ...extra,
  };
}

function report(domain: MockDomain, healthy: boolean): DomainReport {
  const records: RecordCheck[] = [
    record("mx", domain.name, "10 mail.uwu.example", ["10 mail.uwu.example"]),
    record("spf", domain.name, "v=spf1 a:mail.uwu.example -all", healthy ? ["v=spf1 a:mail.uwu.example -all"] : [], {
      status: healthy ? "ok" : "missing",
    }),
    record(
      "dmarc",
      `_dmarc.${domain.name}`,
      `v=DMARC1; p=quarantine; adkim=s; aspf=s; rua=mailto:postmaster@${domain.name}`,
      ["v=DMARC1; p=none"],
      { note: "dmarcNone" },
    ),
    ...domain.keys
      .filter((k) => k.state !== "retired")
      .map((k) =>
        record("dkim", k.dnsName, k.dnsValue, healthy || k.state === "active" ? [k.dnsValue] : [], {
          selector: k.selector,
          keyState: k.state,
          status: healthy || k.state === "active" ? "ok" : "missing",
        }),
      ),
  ];
  const order = ["ok", "warning", "missing", "wrong", "error"];
  const status = records
    .filter((r) => r.keyState !== "pending")
    .reduce<RecordCheck["status"]>(
      (worst, r) => (order.indexOf(r.status) > order.indexOf(worst) ? r.status : worst),
      "ok",
    );
  return {
    domain: domain.name,
    checkedAt: Math.floor(Date.now() / 1000),
    source: "authoritative",
    nameservers: ["ns1.dns.example", "ns2.dns.example"],
    status,
    records,
  };
}

const domains: MockDomain[] = [
  {
    name: "uwu.example",
    catchAll: null,
    createdAt: now - 30 * 86_400,
    keys: [
      key("uwu.example", "uwu202609r", "active", "rsa-sha256"),
      key("uwu.example", "uwu202609e", "active", "ed25519-sha256"),
    ],
    report: null,
  },
  {
    name: "verein.example",
    catchAll: "kassenwart@verein.example",
    createdAt: now - 20 * 86_400,
    keys: [
      key("verein.example", "uwu202608r", "active", "rsa-sha256"),
      key("verein.example", "uwu202608e", "active", "ed25519-sha256"),
    ],
    report: null,
  },
];
domains[0]!.report = report(domains[0]!, true);
let rotationChecks = 0;

const addressCount = (domain: string, kind: "primary" | "alias") =>
  people
    .filter((p) => p.status !== "deleted")
    .flatMap((p) => p.addresses)
    .filter((a) => a.kind === kind && a.address.endsWith(`@${domain}`)).length;

const summary = (domain: MockDomain): DomainSummary => ({
  name: domain.name,
  catchAll: domain.catchAll,
  createdAt: domain.createdAt,
  people: addressCount(domain.name, "primary"),
  aliases: addressCount(domain.name, "alias"),
  dns: domain.report && { status: domain.report.status, checkedAt: domain.report.checkedAt },
});

const detail = (domain: MockDomain): DomainDetail => ({
  ...summary(domain),
  keys: domain.keys,
  report: domain.report,
  setup: { hostname: "mail.uwu.example", relayHost: null, upstreamMx: false },
});

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

const queue = [
  {
    id: 41,
    from: "leni@uwu.example",
    size: 48_213,
    createdAt: now - 3 * 3600,
    expiresAt: now + 4 * 86_400,
    recipients: [
      {
        address: "oma@web.example",
        status: "pending",
        attempts: 3,
        nextAttemptAt: now + 1500,
        lastError: "451 4.7.1 Greylisted, please try again later",
      },
      { address: "opa@web.example", status: "delivered", attempts: 1, nextAttemptAt: now, lastError: null },
    ],
  },
  {
    id: 42,
    from: "",
    size: 3_120,
    createdAt: now - 600,
    expiresAt: now + 5 * 86_400,
    recipients: [
      {
        address: "spammer@bad.example",
        status: "pending",
        attempts: 1,
        nextAttemptAt: now + 240,
        lastError: "connection timed out",
      },
    ],
  },
];

let logSeq = 0;
const LOG_SAMPLES: [string, string, [string, string][]][] = [
  [
    "info",
    "received message",
    [
      ["from", "news@shop.example"],
      ["recipients", "1"],
    ],
  ],
  [
    "info",
    "web login",
    [
      ["login", "lorin@uwu.example"],
      ["ip", "192.0.2.10"],
    ],
  ],
  [
    "info",
    "delivered",
    [
      ["to", "opa@web.example"],
      ["reply", "250 2.0.0 Ok"],
    ],
  ],
  [
    "warn",
    "failed web login",
    [
      ["login", "admin@uwu.example"],
      ["ip", "203.0.113.9"],
    ],
  ],
  [
    "info",
    "delivery deferred",
    [
      ["to", "oma@web.example"],
      ["error", "451 4.7.1 Greylisted"],
    ],
  ],
  ["error", "writing the change log failed", [["err", "database is locked"]]],
];

const settings: Record<string, { value: unknown; source: "default" | "database" | "file"; set?: boolean }> = {
  "tone.language": { value: "de", source: "file" },
  "tone.internal": { value: "playful", source: "default" },
  "tone.external": { value: "neutral", source: "default" },
  "delivery.relay.host": { value: "relay.example.net", source: "database" },
  "delivery.relay.port": { value: 587, source: "database" },
  "delivery.relay.security": { value: "starttls", source: "database" },
  "delivery.relay.username": { value: "uwumail", source: "database" },
  "delivery.relay.password": { value: null, source: "database", set: true },
  "delivery.require_tls": { value: false, source: "default" },
  "delivery.max_lifetime_hours": { value: 120, source: "default" },
  "smtp.max_message_size": { value: 52_428_800, source: "default" },
  "smtp.max_recipients": { value: 100, source: "default" },
  "smtp.verify_senders": { value: true, source: "default" },
  "smtp.enforce_dmarc_reject": { value: true, source: "default" },
  "smtp.require_tls_for_auth": { value: true, source: "default" },
  "smtp.reveal_client_ip": { value: false, source: "default" },
  "smtp.trusted_relays": { value: ["192.0.2.16"], source: "file" },
};

const settingsView = () => ({
  settings: Object.entries(settings).map(([key, entry]) => ({
    key,
    value: entry.value,
    set: entry.set ?? entry.value !== null,
    source: entry.source,
  })),
  configFile: "/etc/uwumail/uwumail.toml",
});

const mockSecurity: SecurityView = {
  totp: false,
  passkeys: [],
  recoveryCodesLeft: 0,
  secondFactor: false,
  appsNeedAppPassword: false,
  appPasswordsRequired: false,
  appPasswords: [
    {
      id: 1,
      name: "Handy",
      scopes: ["mail", "smtp"],
      createdAt: now - 40 * 86_400,
      expiresAt: null,
      lastUsedAt: now - 240,
      lastUsedProtocol: "jmap",
      lastUsedIp: "198.51.100.23",
    },
    {
      id: 2,
      name: "Drucker im Flur",
      scopes: ["smtp"],
      createdAt: now - 10 * 86_400,
      expiresAt: now + 80 * 86_400,
      lastUsedAt: null,
      lastUsedProtocol: null,
      lastUsedIp: null,
    },
  ],
  sessions: [
    {
      id: "a1b2c3d4e5f60718",
      createdAt: now - 3600,
      lastSeenAt: now - 30,
      ip: "192.0.2.10",
      userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:143.0) Gecko/20100101 Firefox/143.0",
      current: true,
    },
    {
      id: "0f1e2d3c4b5a6978",
      createdAt: now - 5 * 86_400,
      lastSeenAt: now - 2 * 86_400,
      ip: "198.51.100.23",
      userAgent:
        "Mozilla/5.0 (iPhone; CPU iPhone OS 26_0 like Mac OS X) AppleWebKit/605.1.15 Version/26.0 Mobile/15E148 Safari/604.1",
      current: false,
    },
  ],
  events: [
    { id: 4, at: now - 3600, kind: "login", actor: "", ip: "192.0.2.10", details: { method: "password" } },
    {
      id: 3,
      at: now - 2 * 86_400,
      kind: "mainPasswordRefused",
      actor: "",
      ip: "203.0.113.9",
      details: { protocol: "smtp" },
    },
    {
      id: 2,
      at: now - 10 * 86_400,
      kind: "appPasswordCreated",
      actor: "",
      ip: "192.0.2.10",
      details: { name: "Drucker im Flur" },
    },
    {
      id: 1,
      at: now - 40 * 86_400,
      kind: "appPasswordCreated",
      actor: "",
      ip: "192.0.2.10",
      details: { name: "Handy" },
    },
  ],
};
let nextSecurityId = 100;
const mockCodes = () => Array.from({ length: 10 }, (_, i) => `k${i}m4p-7qx${i}z`);

function securityEvent(kind: string, details: Record<string, unknown> = {}) {
  mockSecurity.events.unshift({
    id: nextSecurityId++,
    at: Math.floor(Date.now() / 1000),
    kind,
    actor: "",
    ip: "192.0.2.10",
    details,
  });
}

function refreshSecurity() {
  mockSecurity.secondFactor = mockSecurity.totp || mockSecurity.passkeys.length > 0;
  mockSecurity.appPasswordsRequired = mockSecurity.secondFactor || mockSecurity.appsNeedAppPassword;
  if (!mockSecurity.secondFactor) mockSecurity.recoveryCodesLeft = 0;
}

/** A QR-code-looking pattern: the three finder squares and noise, enough to see the layout. */
function fakeQr(size = 29) {
  let modules = "";
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const finder = (fx: number, fy: number) => {
        const dx = x - fx;
        const dy = y - fy;
        if (dx < 0 || dy < 0 || dx > 6 || dy > 6) return null;
        const ring = Math.max(Math.abs(dx - 3), Math.abs(dy - 3));
        return ring === 3 || ring <= 1;
      };
      const inFinder = finder(0, 0) ?? finder(size - 7, 0) ?? finder(0, size - 7);
      modules += (inFinder ?? (x * 7 + y * 13 + x * y) % 3 === 0) ? "1" : "0";
    }
  }
  return { size, modules };
}

let healthCheckedAt: number | null = null;

function health(): Health {
  const at = Math.floor(Date.now() / 1000);
  const order = ["ok", "unknown", "warning", "problem"] as const;
  const area = (name: HealthArea["area"], findings: HealthFinding[]): HealthArea => ({
    area: name,
    level: findings.reduce<HealthArea["level"]>(
      (worst, f) => (order.indexOf(f.level) > order.indexOf(worst) ? f.level : worst),
      "ok",
    ),
    findings,
  });

  const dns: HealthFinding[] = [];
  const pending = domains.filter((d) => !d.report).length;
  for (const domain of domains) {
    const status = domain.report?.status;
    if (!status || status === "ok") continue;
    const level = status === "warning" ? "warning" : status === "error" ? "unknown" : "problem";
    dns.push({
      code: "dnsDomain",
      level,
      params: { domain: domain.name, status },
      link: `/admin/domains/${domain.name}`,
    });
  }
  if (pending > 0) dns.push({ code: "dnsPending", level: "unknown", params: { count: pending } });
  if (dns.length === 0) dns.push({ code: "dnsOk", level: "ok", params: { count: domains.length } });

  const delivery: HealthFinding[] = [
    { code: "relayOk", level: "ok", params: { host: "relay.example.net", lastDeliveredAt: at - 1260 } },
  ];
  const stuck = queue.filter(
    (m) => m.createdAt < at - 3600 && m.recipients.some((r) => r.status === "pending" && r.attempts > 0),
  );
  if (stuck.length > 0) {
    const oldest = Math.min(...stuck.map((m) => m.createdAt));
    delivery.push({
      code: "queueStuck",
      level: "warning",
      params: { count: stuck.length, oldestAt: oldest, ageSecs: at - oldest },
      link: "/admin/queue",
    });
  }

  const storage: HealthFinding[] = [
    { code: "diskOk", level: "ok", params: { freeBytes: 61 * GB, totalBytes: 80 * GB } },
  ];
  const full = people.filter((p) => p.status !== "deleted" && p.quotaBytes > 0 && p.usedBytes >= p.quotaBytes * 0.9);
  if (full.length > 0) {
    storage.push({
      code: "mailboxesNearlyFull",
      level: "warning",
      params: { count: full.length, login: full[0]!.login },
      link: full.length === 1 ? `/admin/people/${full[0]!.login}` : "/admin/people",
    });
  }

  const areas = [
    area("dns", dns),
    area("certificate", [{ code: "certOkAutomatic", level: "ok", params: { days: 71, notAfter: at + 71 * 86_400 } }]),
    area("delivery", delivery),
    area("storage", storage),
    area(
      "security",
      mockSecurity.secondFactor
        ? [{ code: "adminsSecure", level: "ok" }]
        : [{ code: "youWithoutSecondFactor", level: "warning", link: "/account/security" }],
    ),
  ];
  const level = areas.reduce<Health["level"]>(
    (worst, a) => (order.indexOf(a.level) > order.indexOf(worst) ? a.level : worst),
    "ok",
  );
  return { level, checkedAt: healthCheckedAt, areas };
}

const routes: [string, RegExp, Handler][] = [
  ["GET", /^\/api\/admin\/health$/, () => [200, health()]],
  [
    "POST",
    /^\/api\/admin\/health\/check$/,
    () => {
      for (const domain of domains) domain.report = report(domain, domain.name !== "verein.example");
      healthCheckedAt = Math.floor(Date.now() / 1000);
      return [200, health()];
    },
  ],
  ["GET", /^\/api\/admin\/settings$/, () => [200, settingsView()]],
  [
    "PATCH",
    /^\/api\/admin\/settings$/,
    (body) => {
      const changes = (body as { changes: Record<string, unknown> }).changes;
      for (const [key, value] of Object.entries(changes)) {
        const entry = settings[key];
        if (!entry) return problem(422, "invalid");
        if (entry.source === "file") return problem(409, "settingLocked");
        if (key.endsWith("password")) {
          settings[key] = { value: null, source: value === null ? "default" : "database", set: value !== null };
        } else {
          settings[key] = { value, source: value === null ? "default" : "database" };
        }
      }
      if (settings["delivery.relay.host"]?.value === null) {
        for (const key of Object.keys(settings).filter((k) => k.startsWith("delivery.relay."))) {
          settings[key] = { value: null, source: "default", set: false };
        }
      }
      const logged = Object.fromEntries(
        Object.entries(changes).map(([key, value]) => [
          key,
          key.endsWith("password") && value !== null ? "•••" : value,
        ]),
      );
      log("settings.update", "", logged);
      return [200, settingsView()];
    },
  ],
  ["GET", /^\/api\/admin\/queue$/, () => [200, queue]],
  [
    "POST",
    /^\/api\/admin\/queue\/(\d+)\/retry$/,
    (_, [id]) => {
      log("queue.retry", `#${id}`);
      return [204, null];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/queue\/(\d+)$/,
    (_, [id]) => {
      const index = queue.findIndex((message) => String(message.id) === id);
      if (index >= 0) queue.splice(index, 1);
      log("queue.drop", `#${id}`);
      return [204, null];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/logs$/,
    () => {
      // A few new lines on every poll, as if the server were busy.
      const lines = Array.from({ length: logSeq === 0 ? 40 : 2 }, () => {
        logSeq += 1;
        const [level, message, fields] = LOG_SAMPLES[logSeq % LOG_SAMPLES.length]!;
        return { seq: logSeq, at: Date.now() - (40 - logSeq) * 1000, level, target: "uwumail", message, fields };
      });
      return [200, { lines, latest: logSeq }];
    },
  ],
  ["GET", /^\/api\/info$/, () => [200, { hostname: "mail.uwu.example", setupRequired: false } satisfies Info]],
  ["GET", /^\/api\/session$/, () => [200, loggedIn ? session() : null]],
  [
    "POST",
    /^\/api\/auth\/login$/,
    (body) => {
      const login = (body as { login: string }).login;
      if (login.startsWith("wrong")) return problem(401, "invalidCredentials");
      // Log in as "2fa@…" to see the second step; the code is 123456.
      if (login.startsWith("2fa")) {
        return [200, { secondFactor: { token: "mock-token", totp: true, passkey: true, recoveryCodes: true } }];
      }
      loggedIn = true;
      return [200, session()];
    },
  ],
  [
    "POST",
    /^\/api\/auth\/second-factor$/,
    (body) => {
      const { code } = body as { code: string };
      if (code.replace(/\s/g, "") !== "123456" && !/^\w{5}-\w{5}$/.test(code)) return problem(409, "codeInvalid");
      loggedIn = true;
      return [200, session()];
    },
  ],
  ["POST", /^\/api\/auth\/passkey\/options$/, () => problem(409, "loginExpired")],
  ["GET", /^\/api\/account\/security$/, () => [200, mockSecurity]],
  [
    "POST",
    /^\/api\/account\/password$/,
    (body) => {
      const { current } = body as { current: string };
      if (current.startsWith("falsch")) return problem(409, "wrongPassword");
      securityEvent("passwordChanged");
      mockSecurity.sessions = mockSecurity.sessions.filter((entry) => entry.current);
      return [204, null];
    },
  ],
  [
    "POST",
    /^\/api\/account\/totp$/,
    () => [
      200,
      { secret: "AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHH", uri: "otpauth://totp/UwUMail:lorin%40uwu.example", qr: fakeQr() },
    ],
  ],
  [
    "POST",
    /^\/api\/account\/totp\/confirm$/,
    (body) => {
      if ((body as { code: string }).code.replace(/\s/g, "") !== "123456") return problem(409, "codeInvalid");
      const first = !mockSecurity.secondFactor;
      mockSecurity.totp = true;
      if (first) mockSecurity.recoveryCodesLeft = 10;
      refreshSecurity();
      securityEvent("totpEnabled");
      return [200, { recoveryCodes: first ? mockCodes() : null }];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/totp$/,
    (body) => {
      // Shows the password confirmation: any password works except ones starting with "falsch".
      const password = (body as { password?: string } | null)?.password;
      if (!password) return problem(409, "confirmPassword");
      if (password.startsWith("falsch")) return problem(409, "wrongPassword");
      mockSecurity.totp = false;
      refreshSecurity();
      securityEvent("totpDisabled");
      return [204, null];
    },
  ],
  [
    "POST",
    /^\/api\/account\/recovery-codes$/,
    () => {
      mockSecurity.recoveryCodesLeft = 10;
      securityEvent("recoveryCodesCreated");
      return [200, { recoveryCodes: mockCodes() }];
    },
  ],
  [
    "PUT",
    /^\/api\/account\/apps-need-app-password$/,
    (body) => {
      mockSecurity.appsNeedAppPassword = (body as { on: boolean }).on;
      refreshSecurity();
      securityEvent(mockSecurity.appsNeedAppPassword ? "appsNeedAppPassword" : "appsMayUseMainPassword");
      return [204, null];
    },
  ],
  [
    "POST",
    /^\/api\/account\/app-passwords$/,
    (body) => {
      const input = body as { name: string; scopes: AppPasswordInfo["scopes"]; expiresAt: number | null };
      const appPassword: AppPasswordInfo = {
        id: nextSecurityId++,
        name: input.name.trim(),
        scopes: input.scopes,
        createdAt: Math.floor(Date.now() / 1000),
        expiresAt: input.expiresAt,
        lastUsedAt: null,
        lastUsedProtocol: null,
        lastUsedIp: null,
      };
      mockSecurity.appPasswords.unshift(appPassword);
      securityEvent("appPasswordCreated", { name: appPassword.name });
      return [201, { appPassword, secret: "aaaa-bbbb-cccc-dddd" }];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/app-passwords\/(\d+)$/,
    (_, [id]) => {
      const found = mockSecurity.appPasswords.find((entry) => String(entry.id) === id);
      mockSecurity.appPasswords = mockSecurity.appPasswords.filter((entry) => String(entry.id) !== id);
      if (found) securityEvent("appPasswordRevoked", { name: found.name });
      return [204, null];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/sessions\/(\w+)$/,
    (_, [id]) => {
      mockSecurity.sessions = mockSecurity.sessions.filter((entry) => entry.id !== id);
      securityEvent("sessionEnded");
      return [204, null];
    },
  ],
  [
    "POST",
    /^\/api\/account\/sessions\/end-others$/,
    () => {
      const ended = mockSecurity.sessions.filter((entry) => !entry.current).length;
      mockSecurity.sessions = mockSecurity.sessions.filter((entry) => entry.current);
      if (ended) securityEvent("sessionsEnded", { count: ended });
      return [200, { ended }];
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
  ["GET", /^\/api\/admin\/domains$/, () => [200, domains.map(summary)]],
  [
    "POST",
    /^\/api\/admin\/domains$/,
    (body) => {
      const name = (body as { name: string }).name.toLowerCase();
      if (domains.some((d) => d.name === name)) return problem(409, "conflict");
      const created: MockDomain = {
        name,
        catchAll: null,
        createdAt: Math.floor(Date.now() / 1000),
        keys: [key(name, "uwu202609r", "active", "rsa-sha256"), key(name, "uwu202609e", "active", "ed25519-sha256")],
        report: null,
      };
      domains.push(created);
      log("domain.create", name);
      return [201, detail(created)];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/domains\/([^/]+)$/,
    (_, [name]) => {
      const found = domains.find((d) => d.name === name);
      return found ? [200, detail(found)] : problem(404, "notFound");
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/domains\/([^/]+)$/,
    (_, [name]) => {
      const index = domains.findIndex((d) => d.name === name);
      if (index < 0) return problem(404, "notFound");
      if (addressCount(name!, "primary") + addressCount(name!, "alias") > 0) return problem(409, "domainInUse");
      domains.splice(index, 1);
      log("domain.remove", name!);
      return [204, null];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/domains\/([^/]+)\/check$/,
    (_, [name]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      // The second check after a rotation sees the new keys, like after publishing them.
      const pending = found.keys.some((k) => k.state === "pending");
      if (pending) rotationChecks += 1;
      found.report = report(found, !pending || rotationChecks > 1);
      return [200, found.report];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/catch-all$/,
    (body, [name]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      found.catchAll = (body as { login: string | null }).login;
      log("domain.catchAll", name!, { account: found.catchAll });
      return [200, detail(found)];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/domains\/([^/]+)\/dkim\/rotate$/,
    (_, [name]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      if (!found.keys.some((k) => k.state === "pending")) {
        found.keys.push(
          key(name!, "uwu202609br", "pending", "rsa-sha256"),
          key(name!, "uwu202609be", "pending", "ed25519-sha256"),
        );
        rotationChecks = 0;
      }
      found.report = null;
      log("domain.dkimPrepare", name!);
      return [200, detail(found)];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/domains\/([^/]+)\/dkim\/activate$/,
    (body, [name]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      if (!(body as { force: boolean }).force && rotationChecks < 2) return problem(409, "keysNotPublished");
      found.keys = found.keys.map((k) =>
        k.state === "active"
          ? { ...k, state: "retired", retiredAt: Math.floor(Date.now() / 1000) }
          : k.state === "pending"
            ? { ...k, state: "active" }
            : k,
      );
      found.report = null;
      log("domain.dkimActivate", name!);
      return [200, detail(found)];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/domains\/([^/]+)\/dkim\/([^/]+)$/,
    (_, [name, selector]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      found.keys = found.keys.filter((k) => k.selector !== selector);
      log("domain.dkimRemove", name!, { selector });
      return [200, detail(found)];
    },
  ],
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
      if (!found) return problem(404, "notFound");
      const me = found.login === "lorin@uwu.example";
      const security = me
        ? {
            secondFactor: mockSecurity.secondFactor,
            totp: mockSecurity.totp,
            passkeys: mockSecurity.passkeys.length,
            appPasswords: mockSecurity.appPasswords.length,
            appPasswordsRequired: mockSecurity.appPasswordsRequired,
          }
        : found.login === "leni@uwu.example"
          ? { secondFactor: true, totp: true, passkeys: 1, appPasswords: 2, appPasswordsRequired: true }
          : { secondFactor: false, totp: false, passkeys: 0, appPasswords: 0, appPasswordsRequired: false };
      return [200, { ...found, security }];
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
    "POST",
    /^\/api\/admin\/people\/([^/]+)\/reset-second-factors$/,
    (_, [login]) => {
      log("account.secondFactorsReset", login ?? "");
      return [204, null];
    },
  ],
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
