/**
 * A pretend server for `pnpm dev --mode mock`: every page with sample data, no real server
 * and no password needed. Add `?loggedOut` to the URL to see the login page; the password
 * page works with any token except "expired". `?setup` starts on a server without an admin:
 * every setup code except one starting with "wrong" works, and the Cloudflare token "wrong" fails.
 * A login at the OpenID Connect provider is a page of its own and cannot be played here, but what
 * the server sends back can: `/login?oidcError=refused` or `/login?pending=mock&methods=totp,recovery`.
 * `/oauth/authorize?client_id=uwu-thunderbird&redirect_uri=http://127.0.0.1:5000/&scope=openid+mail+smtp`
 * shows the consent page of an app; the client id "uwu-unknown" is not registered, "uwu-known" was
 * allowed before.
 * Production builds never include this file.
 */

import type { Brand } from "@/state/brand";
import type { SignatureChange } from "@/features/mailbox/signatures";
import type {
  AccountMaskedPolicy,
  AccountSpamView,
  AdminAlert,
  AlertsView,
  StatsRange,
  StatsView,
  AntivirusTest,
  AntivirusView,
  EgressTest,
  EgressView,
  BackupSnapshot,
  BackupsView,
  BackupTarget,
  MailboxRestoreView,
  MoveJob,
  MovingView,
  ForwardAddress,
  GroupInfo,
  GreylistHold,
  GreylistView,
  UpdatesView,
  SpamLimits,
  SpamLimitsView,
  AdminSpamView,
  AppPasswordInfo,
  AuthSource,
  FetchAccountInfo,
  FetchView,
  ForwardingView,
  AuditRecord,
  Health,
  HealthArea,
  HealthFinding,
  IdentityInfo,
  DkimKeyInfo,
  DomainDetail,
  DomainKind,
  DomainMaskedPolicy,
  DomainReport,
  DomainSummary,
  EffectiveMaskedPolicy,
  LearnedFromFolders,
  LokiStatus,
  MaskedAddress,
  MaskedState,
  NewSender,
  SenderListEntry,
  SendersView,
  GatewayHostChange,
  GatewayView,
  FeedsView,
  Info,
  MtaStsView,
  OAuthGrantInfo,
  OAuthRequest,
  OwnAddressesView,
  Overview,
  AppPasswordCreated,
  AppScope,
  Person,
  Protocols,
  Profile,
  Reachability,
  RecordCheck,
  HostView,
  ReportDetail,
  ReportEntry,
  ReportKind,
  ReportsOverview,
  ReportsView,
  SentTlsReports,
  SpamLogEntry,
  SpamLogView,
  SecurityView,
  ServerCheck,
  Session,
  SetupStatus,
  SharedMailboxMember,
  WhoMaySend,
  ShareLevel,
  SharingView,
  CalendarsView,
  ShareRights,
  StorageView,
  VacationView,
  WordEntry,
  WordImport,
  WordSource,
  WordsView,
  PictureFile,
  PictureVisibility,
  BimiView,
  MicrosoftChecklist,
  MicrosoftIssue,
  MicrosoftIssues,
} from "@/lib/api";
import { guessSenderKind } from "@/features/spam/senders";
import { assistMockRoutes } from "./mockAssist";
import { ruleRoutes } from "./mockRules";
import { mockChangeSignatures, mockSignatureOverview } from "./mockSignatures";

const now = Math.floor(Date.now() / 1000);
const GB = 1024 ** 3;
const startParams = new URLSearchParams(window.location.search);
let setupOpen = startParams.has("setup");
let loggedIn = !startParams.has("loggedOut") && !setupOpen;
let preferences: Record<string, unknown> = {};

/** The brand as the settings make it; `?brand` in the address starts with a made-up one. */
let mockLogo: string | null = null;
const brand = (): Brand => {
  const name = String(settings["brand.name"]?.value ?? "").trim();
  const color = String(settings["brand.color"]?.value ?? "")
    .trim()
    .toLowerCase();
  const mascot = settings["brand.mascot"]?.value !== false;
  return {
    name: name || "UwUMail",
    custom: Boolean(name || color || !mascot || mockLogo),
    color: color || null,
    mascot,
    logo: mockLogo,
  };
};

/** Roughly what the server derives; the mock only needs something to show. */
function mockPalette(color: string) {
  return {
    light: {
      "--uwu-pink": color,
      "--uwu-pink-solid": color,
      "--uwu-pink-solid-hover": color,
      "--uwu-pink-ink": color,
      "--uwu-pink-tint": `color-mix(in oklch, ${color} 12%, white)`,
      "--uwu-pink-tint-strong": `color-mix(in oklch, ${color} 22%, white)`,
    },
    dark: {
      "--uwu-pink": color,
      "--uwu-pink-solid": color,
      "--uwu-pink-solid-hover": color,
      "--uwu-pink-ink": `color-mix(in oklch, ${color} 70%, white)`,
      "--uwu-pink-tint": `color-mix(in oklch, ${color} 25%, #1c171f)`,
      "--uwu-pink-tint-strong": `color-mix(in oklch, ${color} 35%, #1c171f)`,
    },
  };
}

const session = (): Session => ({
  account: { id: 1, login: "lorin@uwu.example", name: "Lorin", role: "admin" },
  csrfToken: "mock",
  preferences,
  server: { hostname: "mail.uwu.example", version: "0.1.0", brand: brand() },
  webmail: true,
});

const address = (value: string, kind: "primary" | "alias" = "primary") => ({
  address: value,
  kind,
  createdAt: now - 86_400,
});

/** Everything on for a person, calendars and contacts off for a service. */
const allProtocols = (): Protocols => ({ smtp: true, imap: true, jmap: true, caldav: true, carddav: true });
const serviceProtocols = (): Protocols => ({ ...allProtocols(), caldav: false, carddav: false });

function person(login: string, name: string, extra: Partial<Person> = {}): Person {
  return {
    login,
    name,
    role: "user",
    protocols: allProtocols(),
    redirectTo: "",
    hasMailbox: true,
    webmail: true,
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
  person("support@uwu.example", "Support", {
    role: "service",
    protocols: serviceProtocols(),
    sharedMailbox: true,
    quotaBytes: 10 * GB,
  }),
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
  person("backup@uwu.example", "Backup-Skript", {
    role: "service",
    protocols: { smtp: true, imap: false, jmap: false, caldav: false, carddav: false },
    hasMailbox: false,
    redirectTo: "lorin@uwu.example",
    quotaBytes: 0,
    usedBytes: 0,
  }),
  person("noreply@verein.example", "Vereins-Rundmail", {
    role: "service",
    protocols: { smtp: true, imap: true, jmap: true, caldav: false, carddav: false },
    quotaBytes: 1 * GB,
    usedBytes: Math.round(0.12 * GB),
  }),
  person("alt@uwu.example", "Altes Konto", {
    status: "deleted",
    deletedAt: now - 3 * 86_400,
    purgeAt: now + 27 * 86_400,
  }),
];

/** App passwords of the services, as the admin panel sees them. */
const servicePasswords: Record<string, AppPasswordInfo[]> = {
  "backup@uwu.example": [
    {
      id: 901,
      name: "Access",
      scopes: ["smtp"],
      createdAt: now - 9 * 86_400,
      expiresAt: null,
      lastUsedAt: now - 3600,
      lastUsedProtocol: "smtp",
      lastUsedIp: "192.0.2.10",
    },
  ],
};

let nextAppPasswordId = 950;

function newAppPassword(login: string, name: string): AppPasswordCreated {
  const found = people.find((p) => p.login === login);
  const protocols = found?.protocols ?? allProtocols();
  const scopes: AppScope[] = [];
  if (protocols.imap || protocols.jmap) scopes.push("mail");
  if (protocols.smtp) scopes.push("smtp");
  if (protocols.caldav || protocols.carddav) scopes.push("dav");
  const appPassword: AppPasswordInfo = {
    id: (nextAppPasswordId += 1),
    name,
    scopes,
    createdAt: Math.floor(Date.now() / 1000),
    expiresAt: null,
    lastUsedAt: null,
    lastUsedProtocol: null,
    lastUsedIp: null,
  };
  servicePasswords[login] = [appPassword, ...(servicePasswords[login] ?? [])];
  return { appPassword, secret: "nyuu-mock-pass-word" };
}

interface MockDomain {
  name: string;
  signature?: DomainDetail["signature"];
  selfServiceAliases?: boolean;
  catchAll: string | null;
  createdAt: number;
  keys: DkimKeyInfo[];
  report: DomainReport | null;
  /** False for a domain from the setup assistant until its records are at Cloudflare. */
  published?: boolean;
  mtaSts?: MtaStsView | null;
  forwards?: ForwardAddress[];
  groups?: GroupInfo[];
  kind?: DomainKind;
  maskedPolicy?: DomainMaskedPolicy;
}

function mtaStsView(mode: MtaStsView["mode"], changedAt: number): MtaStsView {
  const policy = `version: STSv1\r\nmode: ${mode}\r\nmx: mail.uwu.example\r\nmax_age: ${mode === "testing" ? 86400 : 604800}\r\n`;
  return {
    mode,
    mx: ["mail.uwu.example"],
    changedAt,
    policy,
    id: mode === "testing" ? "3f9a1c0b7d2e4a6f8b1c" : "a7c2e9d14b6f0a3e5d8c",
  };
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
  const made: RecordCheck = {
    kind,
    name,
    recordType: kind === "mx" ? "MX" : "TXT",
    expected,
    found,
    status: "ok",
    note: null,
    selector: null,
    keyState: null,
    optional: false,
    differs: false,
    ...extra,
  };
  // The same rule the server follows: it works, it is just not our wording.
  return {
    ...made,
    differs:
      extra.differs ??
      ((made.status === "ok" || made.status === "warning") &&
        ["MX", "TXT", "SRV"].includes(made.recordType) &&
        made.found.length > 0 &&
        !made.found.includes(made.expected)),
  };
}

/** A small SVG Tiny PS logo as the server would store it after cleaning it up. */
const MOCK_BIMI_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" version="1.2" baseProfile="tiny-ps" viewBox="0 0 64 64">' +
  "<title>UwU Example</title>" +
  '<rect width="64" height="64" fill="#ffffff"/>' +
  '<circle cx="32" cy="32" r="22" fill="#e11d74"/>' +
  '<path d="M22 28v6a10 10 0 0 0 20 0v-6" fill="none" stroke="#ffffff" stroke-width="5" stroke-linecap="round"/>' +
  "</svg>";

interface MockBimi {
  enabled: boolean;
  svg: string | null;
  title: string;
  svgUpdatedAt: number | null;
  certificate: boolean;
  checkedAt: number | null;
}

/** BIMI per domain; only the first domain has it on. */
const mockBimi = new Map<string, MockBimi>([
  [
    "uwu.example",
    {
      enabled: true,
      svg: MOCK_BIMI_SVG,
      title: "UwU Example",
      svgUpdatedAt: now - 5 * 86_400,
      certificate: true,
      checkedAt: now - 3600,
    },
  ],
]);

const bimiOf = (domain: string): MockBimi =>
  mockBimi.get(domain) ?? {
    enabled: false,
    svg: null,
    title: "",
    svgUpdatedAt: null,
    certificate: false,
    checkedAt: null,
  };

function bimiRecordValue(domain: string): string {
  const state = bimiOf(domain);
  const certificate = state.certificate ? `https://mail.uwu.example/bimi/${domain}.pem` : "";
  return `v=BIMI1; l=https://mail.uwu.example/bimi/${domain}.svg; a=${certificate}`;
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
      `v=DMARC1; p=quarantine; adkim=s; aspf=s; rua=mailto:dmarc-reports@${domain.name}`,
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
  const tlsRpt = `v=TLSRPTv1; rua=mailto:tls-reports@${domain.name}`;
  records.push(
    record("tlsrpt", `_smtp._tls.${domain.name}`, tlsRpt, healthy ? [tlsRpt] : [], {
      status: healthy ? "ok" : "missing",
      optional: !domain.mtaSts,
    }),
  );
  if (domain.mtaSts) {
    const txt = `v=STSv1; id=${domain.mtaSts.id}`;
    records.push(
      record("mtasts", `_mta-sts.${domain.name}`, txt, [txt]),
      record("mtastsHost", `mta-sts.${domain.name}`, "mail.uwu.example", ["mail.uwu.example"], { recordType: "CNAME" }),
      record(
        "mtastsPolicy",
        `https://mta-sts.${domain.name}/.well-known/mta-sts.txt`,
        domain.mtaSts.policy,
        [domain.mtaSts.policy],
        { recordType: "HTTPS" },
      ),
    );
  }
  for (const [kind, service, port] of [
    ["jmap", "_jmap._tcp", 443],
    ["imaps", "_imaps._tcp", 993],
    ["submissions", "_submissions._tcp", 465],
    ["submission", "_submission._tcp", 587],
  ] as const) {
    const value = `0 1 ${port} mail.uwu.example`;
    const present = healthy && kind !== "submissions";
    records.push(
      record(kind, `${service}.${domain.name}`, value, present ? [value] : [], {
        recordType: "SRV",
        status: present ? "ok" : "missing",
        optional: true,
      }),
    );
  }
  // The host name's domain is in a signed zone: DANE for mail to this server.
  if (domain.name === "uwu.example") {
    const tlsa = "3 1 1 8d02536c887482bc34ff54e41d2ba659bf85b341a0a20afadb5813dcfbcf286d";
    records.push(
      record("tlsa", "_25._tcp.mail.uwu.example", tlsa, healthy ? [tlsa] : [], {
        recordType: "TLSA",
        status: healthy ? "ok" : "missing",
        note: healthy ? "tlsaKeyKept" : "tlsaRecommended",
        optional: true,
      }),
    );
  }
  if (bimiOf(domain.name).enabled) {
    const value = bimiRecordValue(domain.name);
    records.push(record("bimi", `default._bimi.${domain.name}`, value, [value], { optional: true }));
  }
  const order = ["ok", "warning", "missing", "wrong", "error"];
  const status = records
    .filter((r) => !r.optional)
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
    mtaSts: mtaStsView("testing", now - 20 * 86_400),
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
  {
    name: "masked.example",
    kind: "masked",
    catchAll: null,
    createdAt: now - 10 * 86_400,
    keys: [
      key("masked.example", "uwu202609r", "active", "rsa-sha256"),
      key("masked.example", "uwu202609e", "active", "ed25519-sha256"),
    ],
    report: null,
  },
];
domains[0]!.report = report(domains[0]!, true);
domains[0]!.maskedPolicy = { mode: "both", maskedDomains: ["masked.example"], defaultDomain: null };
let rotationChecks = 0;

const addressCount = (domain: string, kind: "primary" | "alias") =>
  people
    .filter((p) => p.status !== "deleted")
    .flatMap((p) => p.addresses)
    .filter((a) => a.kind === kind && a.address.endsWith(`@${domain}`)).length;

const summary = (domain: MockDomain): DomainSummary => ({
  name: domain.name,
  kind: domain.kind ?? "mail",
  catchAll: domain.catchAll,
  createdAt: domain.createdAt,
  people: addressCount(domain.name, "primary"),
  aliases: addressCount(domain.name, "alias"),
  dns: domain.report && { status: domain.report.status, checkedAt: domain.report.checkedAt },
});

const detail = (domain: MockDomain): DomainDetail => ({
  ...summary(domain),
  selfServiceAliases: domain.selfServiceAliases ?? domain.name === "uwu.example",
  keys: domain.keys,
  report: domain.report,
  mtaSts: domain.mtaSts ?? null,
  forwards: domain.forwards ?? [],
  groups: domain.groups ?? [],
  maskedPolicy: domain.kind === "masked" ? null : domainPolicy(domain),
  maskedDomainChoices: maskedDomainNames(),
  kindBlockers: domain.kind === "masked" ? null : kindBlockers(domain),
  maskedUsedBy: domain.kind === "masked" ? maskedUsedBy(domain.name) : null,
  maskedInUse: mockMasked.filter((entry) => entry.state !== "deleted" && entry.email.endsWith(`@${domain.name}`))
    .length,
  setup: { hostname: "mail.uwu.example", relayHost: null, upstreamMx: false },
  signature: domain.signature ?? { mode: "off", text: "", html: "" },
});

const mockSendAs: Record<string, string[]> = {};

/** Where each person's password is checked; everyone not in here has it on this server. */
const mockAuthSources: Record<string, AuthSource> = {
  "leni@uwu.example": "ldap",
  "ami@uwu.example": "oidc",
};

const oauthGrant = (id: number, clientName: string, scopes: string[], daysAgo: number): OAuthGrantInfo => ({
  id,
  clientName,
  scopes,
  createdAt: now - daysAgo * 86_400,
  lastUsedAt: daysAgo > 20 ? null : now - 900,
  lastUsedProtocol: daysAgo > 20 ? null : "imap",
  lastUsedIp: daysAgo > 20 ? null : "198.51.100.23",
});

/** Apps signed in with OAuth, per person; the logged-in admin's own are on the security page. */
const mockPersonGrants: Record<string, OAuthGrantInfo[]> = {
  "leni@uwu.example": [oauthGrant(31, "Thunderbird", ["openid", "email", "offline_access", "mail", "smtp"], 3)],
};
let nextGrantId = 200;

/** The apps registered with the OAuth provider, by client id. */
const mockOAuthClients: Record<string, string> = {
  "uwu-thunderbird": "Thunderbird",
  "uwu-known": "K-9 Mail",
};

// The machine's helper: a job runs for a few seconds, printing as it goes, then is done.
let hostJob: { verb: string; asked: number } | null = null;
const JOB_LINES: Record<string, string[]> = {
  "uwumail-update": [
    "== fetching the newest update.sh",
    "  UwUMail Server, update",
    "  running now: 0.1.0",
    "  compose.yaml brought up to date",
    "  backing up first",
    "  fetching the images",
    "  starting the new version",
    "  waiting for the server.....",
    "  the machine's helper is up to date",
    "  UwUMail is on 0.1.1 now (=^･ω･^=)",
  ],
  "helper-update": ["== fetching the newest helper", "  installing the helper", "  installing the units"],
};

function hostView(): HostView {
  const elapsed = hostJob ? (Date.now() - hostJob.asked) / 1000 : 0;
  const lines = hostJob ? (JOB_LINES[hostJob.verb] ?? ["== " + hostJob.verb]) : [];
  const shown = Math.min(lines.length, Math.floor(elapsed / 0.6));
  const state = !hostJob ? null : elapsed < 1 ? "waiting" : shown < lines.length ? "running" : "done";
  return {
    available: true,
    machine: {
      kind: "debian",
      name: "Ubuntu 24.04.1 LTS",
      updates: 7,
      securityUpdates: 3,
      rebootRequired: true,
      rebootPackages: ["linux-image-generic", "libssl3"],
      // The mock shows the case worth showing: this machine is not UwUMail's alone.
      alone: false,
      others: ["caddy"],
      image: "ghcr.io/minifyx/uwumail-server:latest",
      digest: "sha256:0c1d2e",
      composeDir: "/opt/uwumail",
      checkedAt: now - 400,
      helper: new URLSearchParams(window.location.search).get("helper") ?? "3",
      verbs:
        new URLSearchParams(window.location.search).get("helper") === "2"
          ? ["os-update", "reboot", "vpn-apply", "vpn-stop"]
          : ["os-update", "reboot", "uwumail-update", "helper-update", "vpn-apply", "vpn-stop", "vpn-remove"],
    },
    job: hostJob && state ? { id: "18c0ffee", state, error: "", at: Math.floor(hostJob.asked / 1000) } : null,
    log: lines.slice(0, shown).join("\n"),
    command: "",
  };
}

const mockUpdates: UpdatesView = {
  build: { version: "0.1.0", commit: "3eedf6a1c0ffee", release: true },
  settings: { check: true, channel: "stable" },
  info: {
    checkedAt: Math.floor(Date.now() / 1000) - 3 * 3600,
    error: null,
    releases: [
      {
        version: "0.1.1",
        name: "UwUMail Server 0.1.1",
        notes: "- Backups: restore single mailboxes in the portal\n- IMAP: faster SEARCH in big folders",
        publishedAt: "2026-10-02T09:00:00Z",
        url: "https://github.com/example/releases/v0.1.1",
        prerelease: false,
      },
    ],
    behind: null,
    commits: [],
  },
  image: "ghcr.io/minifyx/uwumail-server:latest",
  serverCommand: "sudo bash update.sh",
  gateway: { software: "uwumail-gateway 0.1.0" },
  gatewayCommand:
    "cd /tmp && curl -fsSLO https://github.com/example/releases/download/v0.1.1/uwumail-gateway-linux-amd64.tar.gz && sudo bash uwumail-gateway/install.sh uwumail-gateway/uwumail-gateway",
};

const mockBackups: BackupsView = {
  enabled: true,
  hour: 1,
  minute: 30,
  retention: { daily: 7, weekly: 4, monthly: 6 },
  encrypted: true,
  target: {
    kind: "sftp",
    host: "nas.uwu.example",
    port: 22,
    user: "backup",
    path: "uwumail",
    method: "key",
    publicKey:
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleExampleExampleExampleExampleExample uwumail-backup@mail.uwu.example",
    passwordSet: false,
    hostKey: "SHA256:uwuExampleHostKeyFingerprint0000000000000000",
  },
  status: {
    lastAttemptAt: Math.floor(Date.now() / 1000) - 5 * 3600,
    lastSuccessAt: Math.floor(Date.now() / 1000) - 5 * 3600,
    startedAt: Math.floor(Date.now() / 1000) - 5 * 3600,
    finishedAt: Math.floor(Date.now() / 1000) - 5 * 3600 + 210,
    lastError: null,
    lastReport: {
      snapshot: "001790000000-a1b2c3",
      uploaded: 18_400_000,
      total: 2_310_000_000,
      removedSnapshots: 1,
      removedObjects: 12,
    },
  },
  running: false,
  restore: {
    available: true,
    fetching: { state: "idle", snapshot: "", error: "", startedAt: 0, doneBytes: 0, totalBytes: 0 },
    staged: null,
    last: null,
  },
  mailboxRestore: {
    state: "",
    snapshot: "",
    createdAt: 0,
    error: "",
    doneBytes: 0,
    totalBytes: 0,
    people: [],
    account: "",
    total: 0,
    done: 0,
    restored: 0,
    skipped: 0,
    last: null,
  },
};

let restoreStartedAt = 0;
let mailboxStartedAt = 0;

/** The people of a snapshot in the mock, with a few folders each. */
function snapshotPeople(): MailboxRestoreView["people"] {
  const folders = (base: number) => [
    { id: base + 1, parentId: null, path: ["Inbox"], role: "inbox", emails: 812 },
    { id: base + 2, parentId: null, path: ["Sent"], role: "sent", emails: 240 },
    { id: base + 3, parentId: null, path: ["Verein"], role: null, emails: 96 },
    { id: base + 4, parentId: base + 3, path: ["Verein", "2025"], role: null, emails: 41 },
    { id: base + 5, parentId: null, path: ["Trash"], role: "trash", emails: 12 },
  ];
  return [
    { login: "lorin@uwu.example", name: "Lorin", emails: 1201, folders: folders(0) },
    { login: "mini@uwu.example", name: "Mini", emails: 433, folders: folders(10) },
  ];
}

/** Opening a snapshot and restoring a mailbox take a few seconds in the mock, like a small real one. */
function stepMailbox() {
  const job = mockBackups.mailboxRestore;
  const elapsed = Date.now() - mailboxStartedAt;
  if (job.state === "opening") {
    job.doneBytes = Math.min(job.totalBytes, Math.round((job.totalBytes * elapsed) / 4000));
    if (elapsed > 4000) Object.assign(job, { state: "open", people: snapshotPeople() });
  } else if (job.state === "restoring") {
    job.done = Math.min(job.total, Math.round((job.total * elapsed) / 5000));
    job.skipped = Math.round(job.done * 0.1);
    job.restored = job.done - job.skipped;
    if (elapsed > 5000) {
      job.state = "open";
      const day = new Date(job.createdAt * 1000).toISOString().slice(0, 10);
      job.last = {
        account: job.account,
        into: job.account,
        folder: `Restored ${day}`,
        restored: job.restored,
        skipped: job.skipped,
        error: "",
        finishedAt: Math.floor(Date.now() / 1000),
      };
    }
  }
}

/**
 * A restore in the mock walks through fetching and then waits, which is where a real one leaves the
 * portal: the server stops, and the start after it puts the files in place.
 */
function stepRestore(): BackupsView {
  const restore = mockBackups.restore;
  if (restore.fetching.state !== "fetching") return mockBackups;
  if (Date.now() - restoreStartedAt > 6000) {
    restore.fetching = { ...restore.fetching, state: "ready", doneBytes: restore.fetching.totalBytes };
    restore.staged = {
      snapshot: restore.fetching.snapshot,
      hostname: "mail.old.example",
      createdAt: Math.floor(Date.now() / 1000) - 86_400,
      keepGateway: true,
      askedAt: Math.floor(Date.now() / 1000),
      by: "lorin@uwu.example",
    };
  }
  return mockBackups;
}

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

type Handler = (body: unknown, params: string[], search: URLSearchParams) => [number, unknown];

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
  "brand.name": { value: startParams.has("brand") ? "Post & Co" : "", source: "default" },
  "brand.color": { value: startParams.has("brand") ? "#0ea5e9" : "", source: "default" },
  "brand.mascot": { value: !startParams.has("brand"), source: "default" },
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
  "smtp.allow_external_forwarding": { value: true, source: "default" },
  "spam.enabled": { value: true, source: "default" },
  "spam.blocklists": { value: true, source: "default" },
  "spam.uri_blocklists": { value: false, source: "default" },
  "spam.junk_score": { value: 5, source: "default" },
  "spam.greylist_score": { value: 2, source: "default" },
  "spam.greylist_delay_secs": { value: 300, source: "default" },
  "spam.reject_score": { value: null, source: "default" },
  "spam.bayes": { value: true, source: "default" },
  "spam.feeds.urlhaus": { value: true, source: "default" },
  "spam.feeds.malware_bazaar": { value: true, source: "default" },
  "spam.feeds.bad_subjects": { value: true, source: "default" },
  "spam.feeds.disposable": { value: true, source: "default" },
  "spam.feeds.freemail": { value: true, source: "default" },
  "spam.feeds.redirectors": { value: false, source: "database" },
  "spam.feeds.abuse_ch_key": { value: null, source: "default", set: false },
  "spam.antivirus.enabled": { value: true, source: "database" },
  "spam.antivirus.address": { value: "clamav:3310", source: "default" },
  "spam.antivirus.timeout_secs": { value: 30, source: "default" },
  "spam.antivirus.max_size": { value: 26_214_400, source: "default" },
  "spam.log.enabled": { value: true, source: "default" },
  "spam.log.clean_subjects": { value: false, source: "default" },
  "spam.log.retention_days": { value: 30, source: "default" },
  "egress.proxy": { value: null, source: "database", set: true },
  "egress.fallback": { value: "block", source: "default" },
  "egress.pictures": { value: true, source: "default" },
  "egress.updates": { value: true, source: "database" },
  "egress.fetch": { value: false, source: "default" },
  "egress.image_cache_mb": { value: 1024, source: "default" },
  "egress.assist": { value: false, source: "default" },
  "reports.send_tls_reports": { value: true, source: "default" },
  "log.loki.enabled": { value: false, source: "default" },
  "log.loki.privacy_consent": { value: false, source: "default" },
  "log.loki.url": { value: null, source: "default" },
  "log.loki.username": { value: null, source: "default" },
  "log.loki.password": { value: null, source: "default", set: false },
  "log.loki.token": { value: null, source: "default", set: false },
  "log.loki.tenant": { value: null, source: "default" },
  "log.loki.labels": { value: [], source: "default" },
  "log.loki.level": { value: "info", source: "default" },
  "log.loki.gateway": { value: true, source: "default" },
  "metrics.enabled": { value: false, source: "default" },
  "metrics.token": { value: null, source: "default", set: false },
  "metrics.allowed_networks": { value: [], source: "default" },
  "auth.oidc.enabled": { value: true, source: "database" },
  "auth.oidc.issuer": { value: "https://auth.example.com/application/o/uwumail/", source: "database" },
  "auth.oidc.client_id": { value: "uwumail", source: "database" },
  "auth.oidc.client_secret": { value: null, source: "database", set: true },
  "auth.oidc.button_label": { value: "Authentik", source: "database" },
  "auth.oidc.auto_create": { value: false, source: "default" },
  "auth.oidc.allowed_domains": { value: [], source: "default" },
  "auth.oidc.admin_group_claim": { value: null, source: "default" },
  "auth.oidc.admin_group_value": { value: null, source: "default" },
  "auth.ldap.enabled": { value: true, source: "database" },
  "auth.ldap.url": { value: "ldaps://ldap.example.com", source: "database" },
  "auth.ldap.starttls": { value: true, source: "default" },
  "auth.ldap.insecure_localhost": { value: false, source: "default" },
  "auth.ldap.bind_dn": { value: "cn=uwumail,ou=services,dc=example,dc=com", source: "database" },
  "auth.ldap.bind_password": { value: null, source: "database", set: true },
  "auth.ldap.user_dn_template": { value: null, source: "default" },
  "auth.ldap.base_dn": { value: "ou=people,dc=example,dc=com", source: "database" },
  "auth.ldap.user_filter": { value: "(&(objectClass=person)(mail={email}))", source: "default" },
  "auth.ldap.mail_attribute": { value: "mail", source: "default" },
  "auth.ldap.name_attribute": { value: "cn", source: "default" },
  "auth.ldap.admin_group_dn": { value: null, source: "default" },
  "auth.ldap.auto_create": { value: false, source: "default" },
  "auth.ldap.allowed_domains": { value: [], source: "default" },
  "fetch.oauth.microsoft_client_id": { value: null, source: "default" },
  "fetch.oauth.google_client_id": { value: "1234567890-mock.apps.googleusercontent.com", source: "database" },
  "fetch.oauth.google_client_secret": { value: null, source: "database", set: true },
};

const settingsView = () => ({
  settings: Object.entries(settings).map(([key, entry]) => ({
    key,
    value: entry.value,
    set: entry.set ?? entry.value !== null,
    source: entry.source,
  })),
  configFile: "/etc/uwumail/uwumail.toml",
  // The mock portal has a gateway, so the note on the sending page can be seen while working on it.
  gateway: { paired: true },
});

const senderEntry = (
  id: number,
  list: SenderListEntry["list"],
  value: string,
  note = "",
  domain: string | null = null,
): SenderListEntry => ({
  id,
  list,
  kind: guessSenderKind(value),
  value,
  note,
  domain,
  createdAt: now - id * 86_400,
  createdBy: "mini@uwu.example",
});
const mockSenders: Record<"own" | "admin", SenderListEntry[]> = {
  own: [senderEntry(1, "allow", "oma@example.net"), senderEntry(2, "block", "werbung.example", "Newsletter")],
  admin: [
    senderEntry(10, "block", "198.51.100.0/24", "Hat nur Spam geschickt"),
    senderEntry(11, "allow", "*.mail.partner.example", "Partner", "uwu.example"),
  ],
};
let nextSenderId = 100;
const sendersView = (scope: "own" | "admin"): SendersView => ({
  entries: [...mockSenders[scope]].sort((a, b) => a.list.localeCompare(b.list) || a.value.localeCompare(b.value)),
  limit: scope === "own" ? 1000 : 10_000,
  ...(scope === "admin" ? { domains: ["uwu.example", "verein.example"] } : {}),
});
const addSender = (scope: "own" | "admin", body: unknown): [number, unknown] => {
  const { list, value, kind, note, domain } = body as NewSender;
  const trimmed = value.trim().toLowerCase();
  if (!trimmed.includes(".") && !trimmed.includes(":")) return problem(409, "senderInvalid");
  if (mockSenders[scope].some((entry) => entry.value === trimmed && (entry.domain ?? "") === (domain ?? ""))) {
    return problem(409, "senderListed");
  }
  const entry = { ...senderEntry(nextSenderId++, list, trimmed, note ?? "", domain ?? null), createdAt: now };
  mockSenders[scope].push(kind ? { ...entry, kind } : entry);
  return [201, sendersView(scope)];
};
const removeSender = (scope: "own" | "admin", id: string | undefined): [number, unknown] => {
  mockSenders[scope] = mockSenders[scope].filter((entry) => String(entry.id) !== id);
  return [200, sendersView(scope)];
};

const wordEntry = (
  id: number,
  pattern: string,
  points: number | null = null,
  domain: string | null = null,
): WordEntry => ({
  id,
  pattern,
  points,
  note: "",
  domain,
  createdAt: now - id * 3_600,
  createdBy: "mini@uwu.example",
});
const wordSource = (id: number, url: string, entries: number, error: string | null = null): WordSource => ({
  id,
  url,
  subjectOnly: url.includes("subject"),
  points: null,
  domain: null,
  fetchedAt: now - 5 * 3_600,
  error,
  entries,
  createdAt: now - 20 * 86_400,
  createdBy: "mini@uwu.example",
});
const mockWords: Record<"own" | "admin", WordsView> = {
  own: {
    entries: [wordEntry(1, "gewinnspiel"), wordEntry(2, "/\\sjackpot\\s/i", 4)],
    sources: [],
    limit: 2000,
    sourceLimit: 5,
    defaultPoints: 2.5,
    maxPoints: 10,
  },
  admin: {
    entries: [wordEntry(10, "casino"), wordEntry(11, "web development", 1.5, "verein.example")],
    sources: [
      wordSource(20, "https://lists.example.org/bad_words.map", 29),
      wordSource(21, "https://lists.example.net/subjects.txt", 0, "the link answered 404 Not Found"),
    ],
    domains: ["uwu.example", "verein.example"],
    limit: 20000,
    sourceLimit: 50,
    defaultPoints: 2.5,
    maxPoints: 10,
  },
};
let nextWordId = 500;
const addWords = (scope: "own" | "admin", body: unknown): [number, unknown] => {
  const { text: typed, points, domain } = body as { text: string; points?: number; domain?: string };
  const report: WordImport = { added: 0, duplicates: 0, refused: [], refusedCount: 0 };
  for (const line of typed.split("\n").map((entry) => entry.trim())) {
    if (!line || line.startsWith("#")) continue;
    if (line.includes("(?")) {
      report.refused.push({ line, reason: "look-around is not supported" });
      continue;
    }
    const pattern = line.startsWith("/") ? line : line.toLowerCase();
    if (mockWords[scope].entries.some((entry) => entry.pattern === pattern)) {
      report.duplicates++;
      continue;
    }
    mockWords[scope].entries.push(wordEntry(nextWordId++, pattern, points ?? null, domain ?? null));
    report.added++;
  }
  report.refusedCount = report.refused.length;
  return [200, { import: report, lists: mockWords[scope] }];
};
const subscribeWords = (scope: "own" | "admin", body: unknown): [number, unknown] => {
  const { url, domain } = body as { url: string; domain?: string };
  if (!url.startsWith("https://")) return problem(409, "wordSourceInvalid");
  const source = { ...wordSource(nextWordId++, url, 12), domain: domain ?? null, fetchedAt: now };
  mockWords[scope].sources.push(source);
  return [200, { error: null, lists: mockWords[scope] }];
};
const mockFeeds: FeedsView = {
  abuseChKeySet: false,
  feeds: [
    ["urlhaus", "abuse.ch URLhaus", "https://urlhaus.abuse.ch/api/", true, false, 0, null],
    ["malware_bazaar", "abuse.ch MalwareBazaar", "https://bazaar.abuse.ch/export/", true, false, 0, null],
    ["bad_subjects", "mailcow", "https://github.com/mailcow/mailcow-dockerized", false, true, 125, null],
    ["disposable", "Rspamd", "https://rspamd.com/", false, true, 1161, null],
    ["freemail", "Rspamd", "https://rspamd.com/", false, true, 4009, "the Rspamd list did not answer in time"],
    ["redirectors", "Rspamd", "https://rspamd.com/", false, false, 1211, null],
  ].map(([key, source, page, needsKey, active, entries, error]) => ({
    key: key as string,
    source: source as string,
    page: page as string,
    needsKey: needsKey as boolean,
    intervalSecs: needsKey ? 3_600 : 86_400,
    active: active as boolean,
    fetchedAt: active ? now - 3 * 3_600 : null,
    changedAt: active ? now - 3 * 3_600 : null,
    error: error as string | null,
    entries: entries as number,
  })),
};

const mockBayes = { own: { spam: 18, ham: 41 }, server: { spam: 264, ham: 1310 } };
const mockLimits: SpamLimitsView = {
  own: { junk: null, reject: null },
  server: { junk: 5, reject: null },
  min: 1,
  max: 100,
};

/** Greylisted mail waiting for its recipient to decide. */
const mockGreylist: GreylistHold[] = [
  {
    id: 7,
    at: now - 420,
    address: "nyu@uwu.test",
    envelopeFrom: "bestellung@versand.example",
    headerFrom: "Versand <bestellung@versand.example>",
    subject: "Deine Bestellung ist unterwegs",
    clientIp: "198.51.100.24",
    score: 2.4,
    size: 18_400,
    expiresAt: now + 2 * 86_400,
  },
  {
    id: 6,
    at: now - 5400,
    address: "nyu@uwu.test",
    envelopeFrom: "no-reply@newsletter.example",
    headerFrom: "no-reply@newsletter.example",
    subject: null,
    clientIp: "203.0.113.9",
    score: 3.8,
    size: 64_200,
    expiresAt: now + 2 * 86_400 - 5400,
  },
];

const mockSecurity: SecurityView = {
  totp: false,
  passkeys: [],
  recoveryCodesLeft: 0,
  secondFactor: false,
  appsNeedAppPassword: false,
  appPasswordScopes: ["mail", "smtp", "dav"] as AppScope[],
  appPasswordsRequired: false,
  oauthGrants: [
    {
      id: 21,
      clientName: "Thunderbird",
      scopes: ["openid", "email", "profile", "offline_access", "mail", "smtp", "dav"],
      createdAt: now - 26 * 3600,
      lastUsedAt: now - 120,
      lastUsedProtocol: "imap",
      lastUsedIp: "192.0.2.10",
    },
  ],
  authSource: "local",
  hasPassword: true,
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
    { id: 6, at: now - 1800, kind: "login", actor: "", ip: "192.0.2.10", details: { method: "oidc" } },
    {
      id: 5,
      at: now - 2400,
      kind: "oidcLinked",
      actor: "",
      ip: "192.0.2.10",
      details: { issuer: "https://auth.example.com" },
    },
    { id: 4, at: now - 3600, kind: "login", actor: "", ip: "192.0.2.10", details: { method: "password" } },
    {
      id: 7,
      at: now - 26 * 3600,
      kind: "oauthGranted",
      actor: "",
      ip: "192.0.2.10",
      details: { name: "Thunderbird", scopes: ["openid", "email", "profile", "offline_access", "mail", "smtp", "dav"] },
    },
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

const mockFetchAccounts: FetchAccountInfo[] = [
  {
    id: 1,
    accountId: 1,
    address: "lorin@freemail.example",
    host: "imap.freemail.example",
    port: 993,
    security: "tls",
    username: "lorin@freemail.example",
    afterFetch: "delete",
    fetchJunk: true,
    intervalSecs: 300,
    enabled: true,
    authServId: "",
    smtpHost: "smtp.freemail.example",
    smtpPort: 587,
    smtpSecurity: "starttls",
    sendEnabled: true,
    createdAt: now - 12 * 86_400,
    lastRunAt: now - 180,
    lastOkAt: now - 180,
    lastError: "",
    lastFetched: 2,
    totalFetched: 431,
    backlogAt: null,
    auth: "password",
    loginExpired: false,
    passwordRefused: false,
    signIn: null,
  },
  {
    id: 2,
    accountId: 1,
    address: "lorin@oldmail.example",
    host: "imap.mail.oldmail.example",
    port: 993,
    security: "tls",
    username: "lorin@oldmail.example",
    afterFetch: "markRead",
    fetchJunk: true,
    intervalSecs: 1800,
    enabled: true,
    authServId: "",
    smtpHost: "",
    smtpPort: 587,
    smtpSecurity: "starttls",
    sendEnabled: false,
    createdAt: now - 3 * 86_400,
    lastRunAt: now - 900,
    lastOkAt: now - 6 * 3600,
    lastError: "the provider did not accept the user name and password",
    lastFetched: 0,
    totalFetched: 1204,
    backlogAt: null,
    auth: "password",
    loginExpired: false,
    passwordRefused: false,
    signIn: null,
  },
  // Microsoft stopped taking its password: the row offers the switch to signing in.
  {
    id: 3,
    accountId: 1,
    address: "lorin@hotmail.de",
    host: "outlook.office365.com",
    port: 993,
    security: "tls",
    username: "lorin@hotmail.de",
    afterFetch: "markRead",
    fetchJunk: true,
    intervalSecs: 900,
    enabled: true,
    authServId: "",
    smtpHost: "smtp-mail.outlook.com",
    smtpPort: 587,
    smtpSecurity: "starttls",
    sendEnabled: false,
    createdAt: now - 40 * 86_400,
    lastRunAt: now - 600,
    lastOkAt: now - 9 * 86_400,
    lastError: 'Microsoft no longer accepts passwords for this mailbox ("Basic authentication is disabled")',
    lastFetched: 0,
    totalFetched: 88,
    backlogAt: null,
    auth: "password",
    loginExpired: false,
    passwordRefused: true,
    signIn: "microsoft",
  },
  // Signed in at Google, and Google ended it: the row asks for a new sign-in.
  {
    id: 4,
    accountId: 1,
    address: "lorin.mock@gmail.com",
    host: "imap.gmail.com",
    port: 993,
    security: "tls",
    username: "lorin.mock@gmail.com",
    afterFetch: "markRead",
    fetchJunk: true,
    intervalSecs: 300,
    enabled: true,
    authServId: "",
    smtpHost: "smtp.gmail.com",
    smtpPort: 465,
    smtpSecurity: "tls",
    sendEnabled: true,
    createdAt: now - 20 * 86_400,
    lastRunAt: now - 300,
    lastOkAt: now - 2 * 86_400,
    lastError: "the sign-in at Google has expired or was revoked; sign in again",
    lastFetched: 0,
    totalFetched: 57,
    backlogAt: null,
    auth: "google",
    loginExpired: true,
    passwordRefused: false,
    signIn: null,
  },
];

/**
 * Sign-ins at Microsoft and Google on their way. The mock says "pending" a few times and then
 * "ready", so the device code and the waiting can be seen without a provider.
 */
const mockSignIns = new Map<
  string,
  { provider: "microsoft" | "google"; address: string; switchId: number | null; polls: number }
>();

/** The mock's idea of who runs a domain's mail, as the server's would say. */
function mockProviderOf(address: string): "microsoft" | "google" | null {
  const domain = address.split("@")[1]?.toLowerCase() ?? "";
  if (/^(hotmail|live|outlook)\.[a-z]{2,3}(\.[a-z]{2,3})?$/.test(domain) || domain === "msn.com") return "microsoft";
  if (domain === "gmail.com" || domain === "googlemail.com") return "google";
  // A domain whose mail servers are Microsoft 365's, found by MX on the real server.
  if (domain === "firma.example") return "microsoft";
  return null;
}

/** Moves from other providers: one finished a while ago and one on its way, which the mock walks forward. */
const mockMoves: MoveJob[] = [
  {
    id: 2,
    address: "lorin.alt@gmx.example",
    host: "imap.gmx.example",
    port: 993,
    login: "lorin.alt@gmx.example",
    state: "running",
    error: "",
    errorDetail: "",
    foldersDone: 3,
    foldersTotal: 9,
    messagesDone: 1840,
    messagesTotal: 5210,
    messagesSkipped: 12,
    bytesDone: 212_000_000,
    createdAt: now - 1200,
    startedAt: now - 1200,
    finishedAt: null,
    lastRunAt: now - 60,
  },
  {
    id: 1,
    address: "lorin@oldmail.example",
    host: "imap.oldmail.example",
    port: 993,
    login: "lorin@oldmail.example",
    state: "done",
    error: "",
    errorDetail: "",
    foldersDone: 6,
    foldersTotal: 6,
    messagesDone: 734,
    messagesTotal: 734,
    messagesSkipped: 0,
    bytesDone: 61_000_000,
    createdAt: now - 9 * 86_400,
    startedAt: now - 9 * 86_400,
    finishedAt: now - 9 * 86_400 + 900,
    lastRunAt: now - 9 * 86_400 + 600,
  },
];
let nextMoveId = 3;
const moveStartedAt = new Map<number, number>([[2, Date.now()]]);

/** Every look at the page moves the running moves on a little, and finishes them after a while. */
function stepMoves(): MovingView {
  for (const job of mockMoves) {
    if (job.state !== "queued" && job.state !== "running") continue;
    const elapsed = Date.now() - (moveStartedAt.get(job.id) ?? Date.now());
    if (job.messagesTotal === 0)
      Object.assign(job, { messagesTotal: 1260, foldersTotal: 7, startedAt: Math.floor(Date.now() / 1000) });
    job.state = "running";
    const share = Math.min(1, elapsed / 20_000);
    job.messagesDone = Math.max(job.messagesDone, Math.round(job.messagesTotal * share));
    job.foldersDone = Math.max(job.foldersDone, Math.round(job.foldersTotal * share));
    job.bytesDone = job.messagesDone * 110_000;
    if (share >= 1) Object.assign(job, { state: "done", finishedAt: Math.floor(Date.now() / 1000) });
  }
  return { jobs: mockMoves, max: 5, hasMailbox: true };
}

const mockFetchView = (): FetchView => ({
  accounts: mockFetchAccounts,
  signIn: { microsoft: true, google: true, redirectUri: "https://mail.example.org/api/account/fetch/oauth/callback" },
  max: 10,
  defaultPort: 993,
  defaultIntervalSecs: 300,
  minIntervalSecs: 60,
  maxIntervalSecs: 21_600,
});
const mockForwarding: ForwardingView = {
  keepCopy: true,
  externalAllowed: true,
  maxTargets: 5,
  targets: [
    {
      id: 1,
      address: "hallo@verein.example",
      local: true,
      createdAt: now - 20 * 86_400,
      confirmedAt: now - 20 * 86_400,
    },
    { id: 2, address: "lorin@elsewhere.example", local: false, createdAt: now - 3600, confirmedAt: null },
  ],
};
const mockIdentities: IdentityInfo[] = [
  {
    id: 1,
    name: "Lorin",
    email: "lorin@uwu.example",
    textSignature: "Lorin\nuwu.example",
    htmlSignature: "",
  },
  { id: 2, name: "UwU Verein", email: "verein@uwu.example", textSignature: "", htmlSignature: "" },
];

let mockVacation: VacationView = {
  isEnabled: false,
  fromDate: null,
  toDate: null,
  subject: "Bin im Urlaub",
  textBody: "Hallo, ich bin bis Ende September unterwegs und antworte danach.",
};

const mockAddresses: OwnAddressesView = {
  addresses: [
    { address: "lorin@uwu.example", kind: "primary", own: false, createdAt: now - 30 * 86_400 },
    { address: "hallo@uwu.example", kind: "alias", own: false, createdAt: now - 20 * 86_400 },
    { address: "shop@uwu.example", kind: "alias", own: true, createdAt: now - 4 * 86_400 },
  ],
  domains: ["uwu.example", "verein.example"],
  limit: 10,
  used: 1,
  released: [{ address: "alt-shop@uwu.example", releasedAt: now - 2 * 86_400, reservedUntil: now + 28 * 86_400 }],
  groups: [{ address: "vorstand@verein.example", name: "Vorstand", maySendAs: true }],
  sharedMailboxes: [{ id: 90, address: "support@uwu.example", name: "Support", maySend: true }],
};

const mockMasked: MaskedAddress[] = [
  {
    id: 1,
    email: "maple.otter482@uwu.example",
    state: "enabled",
    forDomain: "https://shop.example.com",
    description: "Online shop",
    url: null,
    emailPrefix: null,
    createdBy: "Portal",
    createdAt: now - 40 * 86_400,
    lastMessageAt: now - 2 * 86_400,
  },
  {
    id: 2,
    email: "news.sunny.wren031@uwu.example",
    state: "disabled",
    forDomain: "https://news.example.net",
    description: "",
    url: null,
    emailPrefix: "news",
    createdBy: "JMAP",
    createdAt: now - 90 * 86_400,
    lastMessageAt: now - 86_400,
  },
];

const MASKED_WORDS = ["maple", "otter", "cloud", "fern", "pebble", "sunny", "wren", "velvet"];

/** What admins set for single accounts; everyone else goes by their domain. */
const mockMaskedCustom: Record<string, AccountMaskedPolicy> = {};
const NO_CUSTOM: AccountMaskedPolicy = { mode: null, maskedDomains: null, defaultDomain: null };

function domainPolicy(domain: MockDomain): DomainMaskedPolicy {
  return domain.maskedPolicy ?? { mode: "off", maskedDomains: [], defaultDomain: null };
}

function maskedDomainNames() {
  return domains.filter((domain) => domain.kind === "masked").map((domain) => domain.name);
}

function kindBlockers(domain: MockDomain) {
  return {
    accounts: people.filter((p) => p.login.endsWith(`@${domain.name}`)).length,
    aliases: addressCount(domain.name, "alias"),
    groups: domain.groups?.length ?? 0,
    forwards: domain.forwards?.length ?? 0,
    catchAll: domain.catchAll !== null,
    sendAs: Object.values(mockSendAs).filter((list) => list.includes(domain.name)).length,
  };
}

function maskedUsedBy(name: string) {
  return {
    domains: domains
      .filter(
        (domain) => domain.maskedPolicy?.maskedDomains.includes(name) || domain.maskedPolicy?.defaultDomain === name,
      )
      .map((domain) => domain.name),
    accounts: Object.entries(mockMaskedCustom)
      .filter(([, custom]) => custom.maskedDomains?.includes(name) || custom.defaultDomain === name)
      .map(([login]) => login)
      .sort(),
  };
}

/** Whether something other than a masked address may go on this domain. */
function maskedOnly(address: string) {
  const name = address.slice(address.lastIndexOf("@") + 1).toLowerCase();
  return domains.some((domain) => domain.name === name && domain.kind === "masked");
}

/** The account's policy in the end, worked out as the server does. */
function effectiveMasked(login: string): EffectiveMaskedPolicy {
  const ownName = login.slice(login.lastIndexOf("@") + 1);
  const own = domains.find((domain) => domain.name === ownName && domain.kind !== "masked");
  const domain = own ? domainPolicy(own) : { mode: "off" as const, maskedDomains: [], defaultDomain: null };
  const custom = mockMaskedCustom[login] ?? NO_CUSTOM;
  const mode = custom.mode ?? domain.mode;
  const maskedDomains = custom.maskedDomains ?? domain.maskedDomains;
  const allowed = new Set<string>();
  if ((mode === "own" || mode === "both") && own) allowed.add(own.name);
  if (mode === "dedicated" || mode === "both") maskedDomains.forEach((name) => allowed.add(name));
  const list = [...allowed].sort();
  const defaultDomain =
    [custom.defaultDomain, domain.defaultDomain].find((name) => name !== null && list.includes(name)) ??
    (own && list.includes(own.name) ? own.name : (list[0] ?? null));
  return { mode, maskedDomains, domains: list, defaultDomain };
}

function personMaskedPolicy(login: string) {
  const ownName = login.slice(login.lastIndexOf("@") + 1);
  const own = domains.find((domain) => domain.name === ownName && domain.kind !== "masked");
  return {
    custom: mockMaskedCustom[login] ?? NO_CUSTOM,
    domain: own ? domainPolicy(own) : null,
    effective: effectiveMasked(login),
    choices: maskedDomainNames(),
  };
}

function maskedView() {
  const policy = effectiveMasked(session().account.login);
  return { addresses: mockMasked, domains: policy.domains, defaultDomain: policy.defaultDomain };
}

/** Members of the shared mailboxes, by their login. */
const sharedMembers: Record<string, SharedMailboxMember[]> = {
  "support@uwu.example": [{ id: 1, login: "lorin@uwu.example", name: "Lorin", maySend: true }],
};

function groupMembers(logins: string[]): GroupInfo["members"] {
  return logins.map((login, index) => ({
    id: index + 1,
    login,
    name: people.find((entry) => entry.login === login)?.name ?? "",
  }));
}
const mockStorage: StorageView = {
  usedBytes: 1.3 * GB,
  quotaBytes: 5 * GB,
  mailboxes: [
    { id: 1, name: "Inbox", role: "inbox", emails: 1840, sizeBytes: 0.8 * GB },
    { id: 2, name: "Drafts", role: "drafts", emails: 3, sizeBytes: 42_000 },
    { id: 3, name: "Sent", role: "sent", emails: 620, sizeBytes: 0.3 * GB },
    { id: 4, name: "Archive", role: "archive", emails: 210, sizeBytes: 0.15 * GB },
    { id: 5, name: "Junk", role: "junk", emails: 57, sizeBytes: 12_000_000 },
    { id: 6, name: "Trash", role: "trash", emails: 133, sizeBytes: 38_000_000 },
    { id: 7, name: "Verein", role: null, emails: 44, sizeBytes: 9_500_000 },
  ],
};

const mockSharing: SharingView = {
  folders: mockStorage.mailboxes.map((mailbox) => ({
    id: mailbox.id,
    path: mailbox.name,
    role: mailbox.role,
    shares:
      mailbox.name === "Verein"
        ? [{ login: "nyu@example.org", name: "Nyu", level: "write" as const, rights: "lrswite" }]
        : [],
  })),
  sharedWithMe: [
    {
      owner: "mini@example.org",
      ownerName: "Mini",
      id: 101,
      path: "Rechnungen",
      role: null,
      level: "read",
      rights: "lr",
    },
  ],
  people: [
    { login: "mini@example.org", name: "Mini" },
    { login: "nyu@example.org", name: "Nyu" },
  ],
};

/** People on the mock server calendars can be shared with, by address. */
const mockPeople = [
  { accountId: 2, address: "mini@example.org", name: "Mini" },
  { accountId: 3, address: "nyu@example.org", name: "Nyu" },
];

const mockCalendars: CalendarsView = {
  calendars: true,
  contacts: true,
  own: [
    {
      id: 1,
      kind: "calendar",
      name: "Persönlich",
      color: "#FF4D8DFF",
      entries: 42,
      shares: [{ accountId: 3, address: "nyu@example.org", name: "Nyu", rights: "read" }],
      isDefault: true,
      subscription: null,
    },
    {
      id: 2,
      kind: "calendar",
      name: "Verein",
      color: "#3BA7FFFF",
      entries: 7,
      shares: [],
      isDefault: false,
      subscription: null,
    },
    {
      id: 4,
      kind: "calendar",
      name: "Schulferien",
      color: "#33B679FF",
      entries: 12,
      shares: [],
      isDefault: false,
      subscription: {
        id: 1,
        collectionId: 4,
        source: "calendar.google.com/…",
        intervalSecs: 3600,
        keepAlarms: false,
        enabled: true,
        nextRunAt: Math.floor(Date.now() / 1000) + 1800,
        lastRunAt: Math.floor(Date.now() / 1000) - 1800,
        lastOkAt: Math.floor(Date.now() / 1000) - 1800,
        lastError: "",
        failures: 0,
        entries: 12,
        createdAt: Math.floor(Date.now() / 1000) - 86_400,
      },
    },
    {
      id: 3,
      kind: "addressbook",
      name: "Kontakte",
      color: null,
      entries: 118,
      shares: [],
      isDefault: true,
      subscription: null,
    },
  ],
  shared: [
    {
      id: 11,
      kind: "calendar",
      name: "Familie",
      color: "#9B6BFFFF",
      owner: "mini@example.org",
      ownerName: "Mini",
      rights: "write",
    },
  ],
  limits: { uploadBytes: 20 * 1024 * 1024, subscriptions: 20, minIntervalSecs: 900 },
};

let mockCollectionId = 100;

/** A collection the mock "imports" into, with a made-up report. */
function mockImport(kind: "calendar" | "addressbook", name: string, target?: number) {
  const existing = target ? mockCalendars.own.find((entry) => entry.id === target) : undefined;
  const collection = existing ?? {
    id: ++mockCollectionId,
    kind,
    name,
    color: null,
    entries: 0,
    shares: [],
    isDefault: false,
    subscription: null,
  };
  if (!existing) mockCalendars.own.push(collection);
  collection.entries += 23;
  const report = {
    total: 25,
    created: 23,
    updated: 0,
    unchanged: 0,
    skipped: 2,
    problems: [
      { item: "Geburtstag Oma", reason: "uidElsewhere" },
      { item: "VFREEBUSY", reason: "unsupportedComponent" },
    ],
    truncated: false,
  };
  return { report, collection: { id: collection.id, kind, name: collection.name } };
}

function refreshAddresses() {
  mockAddresses.used = mockAddresses.addresses.filter((entry) => entry.own).length;
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
  dns.push({
    code: "tlsFailures",
    level: "warning",
    params: { domain: "uwu.example", count: 3 },
    link: "/admin/domains/uwu.example",
  });
  if (pending > 0) dns.push({ code: "dnsPending", level: "unknown", params: { count: pending } });
  if (dns.length === 0) dns.push({ code: "dnsOk", level: "ok", params: { count: domains.length } });

  const delivery: HealthFinding[] = [
    { code: "relayOk", level: "ok", params: { host: "relay.example.net", lastDeliveredAt: at - 1260 } },
  ];
  for (const issue of mockMicrosoftIssues.filter((i) => i.resolvedAt === null)) {
    const code = { blocked: "microsoftBlocked", throttled: "microsoftThrottled", authentication: "microsoftAuth" }[
      issue.kind
    ];
    delivery.push({
      code,
      level: issue.kind === "throttled" ? "warning" : "problem",
      params: {
        ...(issue.scope === "ip" ? { ip: issue.subject } : { domain: issue.subject }),
        code: issue.code,
        count: issue.count,
        lastSeen: issue.lastSeen,
      },
      link: "/admin/microsoft",
    });
  }
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
    ...gatewayArea(area),
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

function gatewayArea(area: (name: HealthArea["area"], findings: HealthFinding[]) => HealthArea): HealthArea[] {
  const view = gatewayView();
  const link = "/admin/mail-flow";
  switch (view.state) {
    case "none":
      return [];
    case "connected":
      return [
        area("gateway", [{ code: "gatewayConnected", level: "ok", params: { addresses: view.addresses }, link }]),
      ];
    case "connecting":
      return [
        area("gateway", [{ code: "gatewayDown", level: "warning", params: { downSince: view.downSince }, link }]),
      ];
    case "refused":
      return [area("gateway", [{ code: "gatewayRefused", level: "problem", params: { refusal: view.refusal }, link }])];
  }
}

let lastServerCheck: ServerCheck | null = null;
const testMails = new Map<string, { sentAt: number; external: string | null }>();

/** The gateway goes from pairing to connected after a few seconds; a code with "wrong" is refused. */
const noGateway: GatewayView = {
  state: "none",
  tunnel: [],
  fingerprint: null,
  addresses: [],
  services: [],
  outboundPorts: [],
  software: null,
  connectedSince: null,
  downSince: null,
  error: null,
  refusal: null,
  fromConfig: false,
  machine: null,
  canInstall: false,
  softwareVersion: null,
};
let gateway: GatewayView = noGateway;
let gatewayPairedAt = 0;
/** Whether the pretend Cloudflare zone points at the gateway already. */
let gatewayDns = false;

let gatewayJobStartedAt = 0;

/** Moves a job on the gateway's machine along, so the buttons can be seen doing something. */
function stepGatewayJob() {
  const job = gateway.machine?.job;
  if (!gateway.machine || !job || job.state !== "running") return;
  const since = Date.now() - gatewayJobStartedAt;
  const machine = gateway.machine;
  if (since > 3000 && !job.log) {
    gateway = {
      ...gateway,
      machine: { ...machine, job: { ...job, log: "== apt-get update\nReading package lists…\n" } },
    };
    return;
  }
  if (since > 9000) {
    gateway = {
      ...gateway,
      machine: {
        ...machine,
        system: machine.system && { ...machine.system, updates: 0, securityUpdates: 0 },
        job: { ...job, state: "done", log: job.log + "\n== apt-get dist-upgrade\n12 upgraded, 0 newly installed.\n" },
      },
    };
  }
}

function gatewayView(): GatewayView {
  const at = Math.floor(Date.now() / 1000);
  stepGatewayJob();
  if (gateway.state === "connecting" && Date.now() - gatewayPairedAt > 4000) {
    gateway = {
      ...gateway,
      state: "connected",
      addresses: ["203.0.113.10", "2001:db8::10"],
      services: ["smtp", "submission", "submissions", "http", "https"],
      outboundPorts: [25, 465, 587],
      software: "uwumail-gateway 0.2.0",
      connectedSince: at,
      downSince: null,
      // A gateway with something to say: updates waiting, a restart due, and the address of the
      // server it must never lock out.
      machine: {
        system: {
          name: "Ubuntu 26.04.1 LTS",
          updates: 12,
          securityUpdates: 3,
          rebootRequired: true,
          automaticSecurity: true,
          newRelease: null,
          command: "apt-get update && apt-get -y dist-upgrade && reboot",
        },
        protection: {
          firewall: "ufw",
          firewallActive: true,
          fail2ban: true,
          banned: 4,
          jails: ["sshd", "recidive", "uwumail-server"],
          fromServer: 1,
        },
        trusted: ["203.0.113.77"],
        checkedAt: at,
        job: null,
      },
      // A gateway with a helper beside it, so the portal shows buttons rather than commands.
      canInstall: true,
      softwareVersion: "0.2.2",
    };
  }
  return gateway;
}

/** A home connection: on the PBL, a made-up reverse name and a closed port 25. */
function reachability(): Reachability {
  const at = Math.floor(Date.now() / 1000);
  const throughGateway = gatewayView().state !== "none";
  return {
    checkedAt: at,
    addresses: [
      {
        ip: "192.0.2.44",
        ptr: ["pc000022c.dip0.isp.example"],
        genericPtr: true,
        homeConnection: true,
        listed: false,
        spamhausUnknown: false,
        asn: 64500,
        network: "Example Broadband AG, DE",
        provider: null,
      },
    ],
    outbound: throughGateway
      ? {
          at,
          route: "gateway",
          target: "mx.example.net:25",
          ok: false,
          stage: "connect",
          error: "timed out",
        }
      : {
          at,
          route: "direct",
          target: "mx.example.net:25",
          ok: false,
          stage: "connect",
          error: "timed out",
        },
    inbound: throughGateway
      ? null
      : { ip: "192.0.2.44", reachable: false, ours: false, greeting: null, error: "timed out" },
    throughGateway,
    recommendation: "gateway",
    reasons: ["homeConnection", "port25Blocked"],
  };
}

/** Direct sending without a relay fails, like on a connection that blocks port 25. */
function serverCheck(blocklists: boolean): ServerCheck {
  const relay = settings["delivery.relay.host"]?.value as string | null;
  const listings = blocklists
    ? [
        { list: "Spamhaus ZEN", status: "unknown" as const, answer: "127.255.255.254" },
        { list: "SpamCop", status: "clean" as const, answer: null },
        { list: "Barracuda", status: "clean" as const, answer: null },
      ]
    : [];
  const at = Math.floor(Date.now() / 1000);
  return {
    checkedAt: at,
    hostname: "mail.uwu.example",
    addresses: [
      {
        ip: "192.0.2.10",
        private: false,
        ptr: ["mail.uwu.example"],
        ptrConfirmed: true,
        ptrIsHostname: true,
        listings: relay ? [] : listings,
      },
      { ip: "2001:db8::10", private: false, ptr: [], ptrConfirmed: false, ptrIsHostname: false, listings: [] },
    ],
    route: relay ? "relay" : "direct",
    relayHost: relay,
    relayAddresses: relay
      ? [{ ip: "198.51.100.25", private: false, ptr: [relay], ptrConfirmed: true, ptrIsHostname: true, listings }]
      : [],
    outbound: relay
      ? { at, route: "relay", target: `${relay}:587`, ok: true, stage: null, error: null }
      : {
          at,
          route: "direct",
          target: "mx.example.net:25",
          ok: false,
          stage: "connect",
          error: "timed out",
        },
    inbound: [
      { ip: "192.0.2.10", reachable: true, ours: true, greeting: "220 mail.uwu.example ESMTP UwUMail", error: null },
      { ip: "2001:db8::10", reachable: false, ours: false, greeting: null, error: "connection refused" },
    ],
    upstream: false,
    blocklistsChecked: blocklists,
  };
}

/** What the spam filter decided, the four kinds that leave no other trace and one that arrived. */
const spamLogEntries: SpamLogEntry[] = [
  {
    id: 413,
    at: now - 300,
    smtpId: "v1r2u5",
    messageId: "<invoice-88@partner.example>",
    action: "virus",
    envelopeFrom: "buchhaltung@partner.example",
    headerFrom: "Buchhaltung <buchhaltung@partner.example>",
    subject: "Rechnung 2026-0912",
    clientIp: "203.0.113.9",
    helo: "mail.partner.example",
    reverseName: "mail.partner.example",
    size: 244_000,
    // A virus is not a matter of points, so the name rides along as a rule of its own.
    score: null,
    hits: [{ rule: "VIRUS", points: 0, detail: "Win.Downloader.Agent-9876543-0" }],
    auth: "Authentication-Results: mail.uwu.example; spf=pass; dkim=pass; dmarc=pass",
    recipients: [{ address: "nyu@uwu.example", action: "virus", mailbox: null }],
    correctedToJunk: null,
  },
  {
    id: 412,
    at: now - 600,
    smtpId: "k3n9x2",
    messageId: "<abc123@spammer.example>",
    action: "reject",
    envelopeFrom: "bounce@spammer.example",
    headerFrom: "Deine Bank <service@bank-sicherheit.example>",
    subject: "Ihr Konto wurde gesperrt – jetzt handeln",
    clientIp: "198.51.100.77",
    helo: "mail.spammer.example",
    reverseName: null,
    size: 18_400,
    score: 14.5,
    hits: [
      { rule: "BAYES_SPAM", points: 4.5, detail: "97 %" },
      { rule: "PHISHING_LINK", points: 4, detail: "bank-sicherheit.example" },
      { rule: "LOOKALIKE_DOMAIN", points: 3, detail: "bank-sicherheit.example ≈ bank.example" },
      { rule: "NO_REVERSE_DNS", points: 1.5, detail: null },
      { rule: "SPAMHAUS_ZEN", points: 1.5, detail: "Spamhaus ZEN" },
    ],
    auth: "Authentication-Results: mail.uwu.example; spf=fail; dkim=none; dmarc=fail",
    recipients: [{ address: "nyu@uwu.example", action: "reject", mailbox: null }],
    correctedToJunk: null,
  },
  {
    id: 411,
    at: now - 4200,
    smtpId: "p8m1qa",
    messageId: "<news-7712@shop.example>",
    action: "junk",
    envelopeFrom: "newsletter@shop.example",
    headerFrom: "Shop Angebote <newsletter@shop.example>",
    subject: "Nur heute: 50 % auf alles",
    clientIp: "203.0.113.9",
    helo: "mail.shop.example",
    reverseName: "mail.shop.example",
    size: 96_200,
    score: 6.2,
    hits: [
      { rule: "BAYES_SPAM", points: 3.5, detail: "88 %" },
      { rule: "MANY_LINKS", points: 1.2, detail: "34 Links" },
      { rule: "FREEMAIL_REPLY_TO", points: 1.5, detail: "antwort@gmx.example" },
    ],
    auth: "Authentication-Results: mail.uwu.example; spf=pass; dkim=pass; dmarc=pass",
    recipients: [{ address: "nyu@uwu.example", action: "junk", mailbox: "junk" }],
    correctedToJunk: false,
  },
  {
    id: 410,
    at: now - 9000,
    smtpId: "v2t7bd",
    messageId: null,
    action: "greylist",
    envelopeFrom: "info@unbekannt.example",
    headerFrom: "info@unbekannt.example",
    subject: "Anfrage",
    clientIp: "192.0.2.201",
    helo: "unbekannt.example",
    reverseName: null,
    size: 3_100,
    score: 2.8,
    hits: [{ rule: "NO_REVERSE_DNS", points: 1.5, detail: null }],
    auth: "Authentication-Results: mail.uwu.example; spf=neutral; dkim=none; dmarc=none",
    recipients: [{ address: "nyu@uwu.example", action: "greylist", mailbox: null }],
    correctedToJunk: null,
  },
  {
    id: 409,
    at: now - 2 * 86_400,
    smtpId: "c4h6we",
    messageId: "<f9a1@freund.example>",
    action: "delivered",
    envelopeFrom: "leni@freund.example",
    headerFrom: "Leni <leni@freund.example>",
    subject: null,
    clientIp: "192.0.2.44",
    helo: "mail.freund.example",
    reverseName: "mail.freund.example",
    size: 7_800,
    score: -0.5,
    hits: [{ rule: "BAYES_HAM", points: -0.5, detail: "4 %" }],
    auth: "Authentication-Results: mail.uwu.example; spf=pass; dkim=pass; dmarc=pass",
    recipients: [{ address: "nyu@uwu.example", action: "delivered", mailbox: "inbox" }],
    correctedToJunk: null,
  },
];

/** The reports themselves, so the list and the detail dialog have something to open. */
function singleReports(kind: ReportKind): ReportEntry[] {
  const dmarc = kind === "dmarc";
  return [
    {
      id: dmarc ? 41 : 22,
      organization: dmarc ? "google.com" : "Google Inc.",
      reportId: dmarc ? "12345678901234567890" : "5065427c-23d3-47ca-b6e0-946ea0e8c4be",
      beginAt: now - 2 * 86_400,
      endAt: now - 86_400,
      receivedAt: now - 3600,
      authenticated: true,
      good: dmarc ? 1237 : 4790,
      bad: dmarc ? 46 : 3,
      about: "uwu.example",
      policy: dmarc ? "quarantine" : null,
    },
    {
      id: dmarc ? 40 : 21,
      organization: dmarc ? "Yahoo" : "Microsoft Corporation",
      reportId: dmarc ? "998877665544332211" : "b1c2d3e4-1111-2222-3333-444455556666",
      beginAt: now - 3 * 86_400,
      endAt: now - 2 * 86_400,
      receivedAt: now - 2 * 86_400,
      authenticated: false,
      good: dmarc ? 128 : 16,
      bad: 0,
      about: "uwu.example",
      policy: dmarc ? "quarantine" : null,
    },
  ];
}

/** What other servers reported about one domain. Only uwu.example has anything to show. */
function reportsFor(name: string): ReportsView {
  const empty = { reports: 0, unauthenticated: 0, firstBegin: null, lastEnd: null, reporters: [] };
  if (name !== "uwu.example") {
    return {
      days: 30,
      dmarc: { ...empty, messages: 0, passed: 0, sources: [] },
      tls: { ...empty, successful: 0, failed: 0, failures: [] },
      suggestions: [],
    };
  }
  return {
    days: 30,
    dmarc: {
      reports: 41,
      unauthenticated: 1,
      messages: 1287,
      passed: 1241,
      firstBegin: now - 30 * 86_400,
      lastEnd: now - 3600,
      reporters: [
        { organization: "google.com", reports: 29, count: 1102 },
        { organization: "Yahoo", reports: 8, count: 131 },
        { organization: "Enterprise Outlook", reports: 4, count: 54 },
      ],
      sources: [
        { ip: "192.0.2.10", messages: 1198, passed: 1198, headerFrom: ["uwu.example"], ours: true },
        { ip: "2001:db8::10", messages: 43, passed: 43, headerFrom: ["uwu.example"], ours: true },
        { ip: "198.51.100.77", messages: 39, passed: 0, headerFrom: ["uwu.example"], ours: false },
        { ip: "203.0.113.9", messages: 7, passed: 0, headerFrom: ["uwu.example"], ours: false },
      ],
    },
    tls: {
      reports: 22,
      unauthenticated: 0,
      successful: 4803,
      failed: 3,
      firstBegin: now - 30 * 86_400,
      lastEnd: now - 3600,
      reporters: [
        { organization: "Google Inc.", reports: 20, count: 4790 },
        { organization: "Microsoft Corporation", reports: 2, count: 16 },
      ],
      failures: [
        { resultType: "certificate-expired", policyType: "sts", mxHost: "mail.uwu.example", sessions: 2 },
        { resultType: "starttls-not-supported", policyType: "sts", mxHost: "mail.uwu.example", sessions: 1 },
      ],
    },
    suggestions: [{ code: "mtaStsEnforce" }, { code: "dmarcStricter", params: { from: "quarantine", to: "reject" } }],
  };
}

/** Admin alerts: a certificate that should have been renewed, a new version, and a full disk that is fine again. */
const mockAlert = (id: number, kind: string, code: string, level: AdminAlert["level"], extra: Partial<AdminAlert>) => ({
  id,
  kind,
  key: code,
  code,
  level,
  params: {},
  link: null,
  firstSeen: now - 3 * 3600,
  lastSeen: now - 60,
  resolvedAt: null,
  notifiedAt: level === "info" ? null : now - 3 * 3600,
  notifiedLevel: level === "info" ? null : level,
  acknowledgedAt: null,
  acknowledgedBy: null,
  ...extra,
});
const mockAlerts: AdminAlert[] = [
  mockAlert(3, "certificate", "certRenewalFailing", "warning", {
    params: { since: now - 30 * 3600, error: "the ACME server could not reach http://mail.uwu.example" },
    link: "/admin/logs",
  }),
  mockAlert(4, "update", "updateAvailable", "info", { params: { version: "0.14.1" }, link: "/admin/updates" }),
  mockAlert(1, "storage", "diskLow", "problem", {
    params: { freeBytes: 0.4 * GB, totalBytes: 32 * GB },
    firstSeen: now - 6 * 86_400,
    lastSeen: now - 5 * 86_400,
    resolvedAt: now - 5 * 86_400 + 900,
  }),
];
const alertsView = (): AlertsView => ({
  open: mockAlerts.filter((alert) => alert.resolvedAt === null),
  resolved: mockAlerts.filter((alert) => alert.resolvedAt !== null),
});

/** Statistics that look like a small family server: a little mail every day, busier on weekdays. */
function mockStats(range: StatsRange): StatsView {
  const DAY = 86_400;
  const dayOf = (at: number) => new Date(at * 1000).toISOString().slice(0, 10);
  const days = Array.from({ length: 366 }, (_, index) => {
    const at = now - (365 - index) * DAY;
    const weekday = new Date(at * 1000).getUTCDay();
    const busy = weekday === 0 || weekday === 6 ? 0.5 : 1;
    const wave = (n: number) => Math.round(n * busy * (0.7 + 0.3 * Math.sin(index * 1.7 + n)));
    // The counting started some weeks ago; before that there is nothing.
    const values: Record<string, number> =
      index < 365 - 200
        ? {}
        : {
            "mail.received": wave(42),
            "mail.junk": wave(6),
            "refused.unknownRecipient": wave(3),
            "refused.spam": wave(9),
            "refused.virus": index % 23 === 0 ? 1 : 0,
            "refused.policy": wave(2),
            "refused.greylisted": wave(5),
            "mail.submitted": wave(12),
            "mail.delivered": wave(15),
            "mail.deferred": index % 9 === 0 ? 3 : 0,
            "mail.bounced": index % 17 === 0 ? 1 : 0,
            "loginFailed.imap": wave(4),
            "loginFailed.smtp": wave(7),
            "loginFailed.portal": index % 5 === 0 ? 1 : 0,
            "gauge.accounts": 7,
            "gauge.storageBytes": Math.round((3.1 + index * 0.004) * GB),
          };
    return { day: dayOf(at), values };
  });
  const periods =
    range === "days"
      ? days.slice(-30).map(({ day, values }) => ({ period: day, values }))
      : days
          .reduce<StatsView["periods"]>((months, { day, values }) => {
            const month = day.slice(0, 7);
            let last = months[months.length - 1];
            if (last?.period !== month) {
              last = { period: month, values: {} };
              months.push(last);
            }
            for (const [key, value] of Object.entries(values)) {
              last.values[key] = key.startsWith("gauge.") ? value : (last.values[key] ?? 0) + value;
            }
            return months;
          }, [])
          .slice(-12);
  const totals: Record<string, number> = {};
  for (const { values } of periods) {
    for (const [key, value] of Object.entries(values)) {
      if (!key.startsWith("gauge.")) totals[key] = (totals[key] ?? 0) + value;
    }
  }
  return { range, periods, totals };
}

/** Where the browser goes back to the app, with the answer in the query as OAuth has it. */
function oauthAnswer(redirectUri: string | null | undefined, answer: Record<string, string>, state?: string | null) {
  // Without an address of the app's own, the mock lands back in the portal, to try again.
  const target = new URL(redirectUri || "/account/security", window.location.origin);
  for (const [key, value] of Object.entries(answer)) target.searchParams.set(key, value);
  if (state) target.searchParams.set("state", state);
  return target.href;
}

/** Checks an app's request the way the server does, roughly: known app, and where it goes back. */
function oauthRequest(params: Record<string, string | undefined>): [number, unknown] {
  const name = mockOAuthClients[params.client_id ?? ""];
  if (!name) return problem(409, "oauthClientUnknown");
  const redirectUri = params.redirect_uri ?? "";
  if (redirectUri.includes("evil")) return problem(409, "oauthRedirectInvalid");
  if (params.response_type !== "code") {
    return [200, { redirect: oauthAnswer(redirectUri, { error: "unsupported_response_type" }, params.state) }];
  }
  const allowed = ["openid", "email", "profile", "offline_access", "mail", "smtp", "dav"];
  const asked = (params.scope ?? "").split(/\s+/).filter((scope) => allowed.includes(scope));
  let redirectHost = "127.0.0.1";
  try {
    if (redirectUri) redirectHost = new URL(redirectUri).hostname;
  } catch {
    return problem(409, "oauthRedirectInvalid");
  }
  const answer: OAuthRequest = {
    client: { name, clientId: params.client_id!, redirectHost },
    scopes: asked.length > 0 ? asked : ["openid", "mail", "smtp"],
    consented: params.client_id === "uwu-known" && params.prompt !== "consent",
  };
  return [200, answer];
}

/** Profile pictures and logos by the address they are managed at; uploads stay in this tab. */
const mockPictures = new Map<
  string,
  { picture: PictureFile | null; visibility: PictureVisibility; sendFace: boolean }
>();
let mockPictureUpload: string | null = null;
let mockPublicPictures = true;
const mockDomainPublic = new Map<string, boolean>();

function pictureView(path: string, own: boolean) {
  const state = mockPictures.get(path) ?? { picture: null, visibility: "server" as PictureVisibility, sendFace: false };
  const mayBePublic = mockPublicPictures;
  return {
    picture: state.picture,
    visibility: state.visibility === "public" && !mayBePublic ? "server" : state.visibility,
    ...(own ? { sendFace: state.sendFace } : {}),
    mayBePublic,
  };
}

function pictureRoutes(pattern: RegExp, own: boolean, face: boolean): [string, RegExp, Handler][] {
  const path = (params: string[]) => params.join("/");
  const current = (params: string[]) =>
    mockPictures.get(path(params)) ?? { picture: null, visibility: "server" as PictureVisibility, sendFace: false };
  return [
    ["GET", pattern, (_, params) => [200, pictureView(path(params), own)]],
    [
      "PUT",
      pattern,
      (_, params) => {
        const url = mockPictureUpload ?? "";
        mockPictures.set(path(params), {
          ...current(params),
          picture: { url, type: "image/png", size: 40_000, updatedAt: Math.floor(Date.now() / 1000) },
        });
        return [200, pictureView(path(params), own)];
      },
    ],
    [
      "DELETE",
      pattern,
      (_, params) => {
        mockPictures.set(path(params), { ...current(params), picture: null });
        return [200, pictureView(path(params), own)];
      },
    ],
    [
      "PATCH",
      pattern,
      (body, params) => {
        const change = body as { visibility?: PictureVisibility; sendFace?: boolean };
        if (change.visibility === "public" && !mockPublicPictures) return problem(409, "publicNotAllowed");
        if (change.sendFace !== undefined && !face) return problem(422, "invalid");
        mockPictures.set(path(params), { ...current(params), ...change });
        return [200, pictureView(path(params), own)];
      },
    ],
  ];
}

function domainLogoView(domain: string) {
  return {
    picture: mockPictures.get(`logo/${domain}`)?.picture ?? null,
    publicPictures: mockDomainPublic.get(domain) ?? true,
    serverAllowsPublic: mockPublicPictures,
  };
}

const pictureMockRoutes: [string, RegExp, Handler][] = [
  ...pictureRoutes(/^\/api\/account\/(picture)$/, true, true),
  ...pictureRoutes(/^\/api\/admin\/people\/([^/]+)\/picture$/, false, false),
  ...pictureRoutes(/^\/api\/admin\/domains\/([^/]+)\/groups\/([^/]+)\/picture$/, false, false),
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/signature$/,
    (body, [domain]) => {
      const found = domains.find((d) => d.name === domain);
      if (!found) return problem(404, "notFound");
      found.signature = body as NonNullable<DomainDetail["signature"]>;
      return [200, found.signature];
    },
  ],
  ["GET", /^\/api\/admin\/domains\/([^/]+)\/logo$/, (_, [domain]) => [200, domainLogoView(domain!)]],
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/logo$/,
    (_, [domain]) => {
      const picture = { url: mockPictureUpload ?? "", type: "image/png", size: 30_000, updatedAt: now };
      mockPictures.set(`logo/${domain}`, { picture, visibility: "server", sendFace: false });
      log("domain.logo", domain!, {});
      return [200, domainLogoView(domain!)];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/domains\/([^/]+)\/logo$/,
    (_, [domain]) => {
      mockPictures.delete(`logo/${domain}`);
      log("domain.logoRemoved", domain!, {});
      return [200, domainLogoView(domain!)];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/public-pictures$/,
    (body, [domain]) => {
      const allowed = (body as { allowed: boolean }).allowed;
      mockDomainPublic.set(domain!, allowed);
      log("domain.publicPictures", domain!, { allowed });
      return [200, domainLogoView(domain!)];
    },
  ],
  ["GET", /^\/api\/admin\/pictures$/, () => [200, { publicAllowed: mockPublicPictures }]],
  [
    "PUT",
    /^\/api\/admin\/pictures$/,
    (body) => {
      mockPublicPictures = (body as { allowed: boolean }).allowed;
      log("pictures.public", "", { allowed: mockPublicPictures });
      return [200, { publicAllowed: mockPublicPictures }];
    },
  ],
];

const mockMicrosoftIssues: MicrosoftIssue[] = [
  {
    id: 2,
    scope: "ip",
    subject: "203.0.113.25",
    kind: "blocked",
    group: "blockList",
    code: "S3150",
    ip: "203.0.113.25",
    domain: "uwu.example",
    reply:
      "550 5.7.1 Unfortunately, messages from [203.0.113.25] weren't sent. Please contact your Internet service " +
      "provider since part of their network is on our block list (S3150). " +
      "[AM0EUR02FT012.eop-EUR02.prod.protection.outlook.com 2026-09-30T08:12:44.000Z]",
    firstSeen: now - 2 * 3600,
    lastSeen: now - 600,
    count: 7,
    resolvedAt: null,
    resolvedBy: null,
  },
  {
    id: 1,
    scope: "ip",
    subject: "203.0.113.25",
    kind: "throttled",
    group: "throttled",
    code: "4.7.650",
    ip: "203.0.113.25",
    domain: "verein.example",
    reply:
      "451 4.7.650 The mail server [203.0.113.25] has been temporarily rate limited due to IP reputation. " +
      "[DB5EUR03FT021.eop-EUR03.prod.protection.outlook.com]",
    firstSeen: now - 9 * 86_400,
    lastSeen: now - 8 * 86_400,
    count: 23,
    resolvedAt: now - 7 * 86_400,
    resolvedBy: "auto",
  },
];

const microsoftIssuesView = (): MicrosoftIssues => ({
  issues: [...mockMicrosoftIssues].sort(
    (a, b) => Number(a.resolvedAt !== null) - Number(b.resolvedAt !== null) || b.lastSeen - a.lastSeen,
  ),
  delistUrl: "https://sender.office.com",
});

let mockChecklistAt = now - 1800;
const microsoftChecklist = (): MicrosoftChecklist => ({
  checkedAt: mockChecklistAt,
  hostname: "mail.uwu.example",
  route: "direct",
  relayHost: null,
  addresses: [
    { ip: "203.0.113.25", private: false, ptr: ["mail.uwu.example"], ptrConfirmed: true, ptrIsHostname: true },
    {
      ip: "2001:db8::25",
      private: false,
      ptr: ["host-25.provider.example"],
      ptrConfirmed: false,
      ptrIsHostname: false,
    },
  ],
  domains: [
    {
      domain: "uwu.example",
      checkedAt: mockChecklistAt,
      spf: "ok",
      dkim: "ok",
      dmarc: "ok",
      dmarcPolicy: "reject",
      dmarcPct: null,
      aligned: "ok",
    },
    {
      domain: "verein.example",
      checkedAt: mockChecklistAt,
      spf: "ok",
      dkim: "ok",
      dmarc: "warning",
      dmarcPolicy: "none",
      dmarcPct: null,
      aligned: "ok",
    },
  ],
  tls: "ok",
});

const microsoftMockRoutes: [string, RegExp, Handler][] = [
  ["GET", /^\/api\/admin\/microsoft\/issues$/, () => [200, microsoftIssuesView()]],
  [
    "POST",
    /^\/api\/admin\/microsoft\/issues\/(\d+)\/resolve$/,
    (_, [id]) => {
      const issue = mockMicrosoftIssues.find((candidate) => candidate.id === Number(id));
      if (!issue) return problem(404, "notFound");
      if (issue.resolvedAt === null) {
        issue.resolvedAt = Math.floor(Date.now() / 1000);
        issue.resolvedBy = "lorin@uwu.example";
        log("microsoft.resolve", issue.subject, { code: issue.code });
      }
      return [200, microsoftIssuesView()];
    },
  ],
  ["GET", /^\/api\/admin\/microsoft\/checklist$/, () => [200, microsoftChecklist()]],
  [
    "POST",
    /^\/api\/admin\/microsoft\/checklist$/,
    () => {
      mockChecklistAt = Math.floor(Date.now() / 1000);
      return [200, microsoftChecklist()];
    },
  ],
];

function bimiView(name: string): BimiView {
  const state = bimiOf(name);
  const found = domains.find((d) => d.name === name);
  const dmarc = found?.report?.records.find((r) => r.kind === "dmarc")?.found[0] ?? null;
  const policy = dmarc ? (/\bp=(\w+)/.exec(dmarc)?.[1] ?? null) : null;
  const pct = dmarc ? Number(/\bpct=(\d+)/.exec(dmarc)?.[1] ?? NaN) : NaN;
  const sp = dmarc ? (/\bsp=(\w+)/.exec(dmarc)?.[1] ?? null) : null;
  // The first domain stands for one that went strict for BIMI.
  const strict = name === "uwu.example";
  const value = bimiRecordValue(name);
  return {
    enabled: state.enabled,
    hasSvg: state.svg !== null,
    title: state.title,
    svgUpdatedAt: state.svgUpdatedAt,
    svgBytes: state.svg?.length ?? null,
    logoUrl: `https://mail.uwu.example/bimi/${name}.svg`,
    certificateUrl: state.certificate ? `https://mail.uwu.example/bimi/${name}.pem` : null,
    certificate: state.certificate
      ? {
          kind: "cmc",
          subject: `CN=UwU Example, O=UwU Example, C=DE`,
          issuer: "CN=Example Mark Certificates CA, O=Example Trust, C=US",
          notBefore: now - 60 * 86_400,
          notAfter: now + 305 * 86_400,
          expired: false,
          names: [name],
          coversDomain: true,
          hasLogotype: true,
        }
      : null,
    record: { name: `default._bimi.${name}`, value },
    published:
      state.enabled && state.checkedAt !== null
        ? { status: "ok", found: [value], note: null, checkedAt: state.checkedAt }
        : null,
    dmarc: strict
      ? {
          status: "ok",
          policy: "reject",
          pct: null,
          subdomainPolicy: null,
          record: "v=DMARC1; p=reject; adkim=s; aspf=s; rua=mailto:dmarc-reports@uwu.example",
        }
      : dmarc
        ? {
            status: policy === "quarantine" || policy === "reject" ? "ok" : "weak",
            policy,
            pct: Number.isNaN(pct) ? null : pct,
            subdomainPolicy: sp,
            record: dmarc,
          }
        : {
            status: found?.report ? "missing" : "unknown",
            policy: null,
            pct: null,
            subdomainPolicy: null,
            record: null,
          },
    domainLogo: Boolean(mockPictures.get(`logo/${name}`)?.picture),
  };
}

function changeBimi(name: string, change: Partial<MockBimi>): [number, unknown] {
  const found = domains.find((d) => d.name === name);
  if (!found) return problem(404, "notFound");
  mockBimi.set(name, { ...bimiOf(name), ...change });
  // The DNS list shows the record while BIMI is on.
  if (found.report) found.report = report(found, found.report.status === "ok");
  return [200, bimiView(name)];
}

const bimiMockRoutes: [string, RegExp, Handler][] = [
  [
    "GET",
    /^\/api\/admin\/domains\/([^/]+)\/bimi$/,
    (_, [name]) => (domains.some((d) => d.name === name) ? [200, bimiView(name!)] : problem(404, "notFound")),
  ],
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/bimi$/,
    (body, [name]) => {
      const change = body as { enabled?: boolean; title?: string };
      if (change.enabled && bimiOf(name!).svg === null) return problem(409, "bimiNoSvg");
      if (change.title !== undefined && !change.title.trim()) return problem(422, "bimiTitle");
      log("domain.bimi", name!, change);
      return changeBimi(name!, {
        ...(change.enabled !== undefined ? { enabled: change.enabled } : {}),
        ...(change.title !== undefined ? { title: change.title.trim() } : {}),
        ...(change.enabled ? { checkedAt: Math.floor(Date.now() / 1000) } : {}),
      });
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/bimi\/svg$/,
    (body, [name]) => {
      const { svg, title, background } = body as { svg: string; title?: string; background?: string | null };
      // Cleaning the SVG is the server's job; the mock only turns away what is plainly not one.
      if (!/<svg[\s>]/.test(svg)) return problem(422, "bimiNotSvg");
      if (/<script/i.test(svg)) return [422, { code: "bimiSvgUnsupported", detail: "script" }];
      if (background && !/^#[0-9a-fA-F]{6}$/.test(background)) return problem(422, "bimiBackground");
      log("domain.bimiLogo", name!, { bytes: svg.length });
      return changeBimi(name!, {
        svg,
        title: title?.trim() || bimiOf(name!).title || name!,
        svgUpdatedAt: Math.floor(Date.now() / 1000),
      });
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/domains\/([^/]+)\/bimi\/svg$/,
    (_, [name]) => {
      log("domain.bimiLogoRemoved", name!, {});
      return changeBimi(name!, { svg: null, svgUpdatedAt: null, enabled: false, checkedAt: null });
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/bimi\/certificate$/,
    (body, [name]) => {
      const pem = (body as { pem: string }).pem;
      if (!pem.includes("-----BEGIN CERTIFICATE-----")) return problem(422, "bimiCertificateInvalid");
      log("domain.bimiCertificate", name!, { kind: "cmc" });
      return changeBimi(name!, { certificate: true });
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/domains\/([^/]+)\/bimi\/certificate$/,
    (_, [name]) => {
      log("domain.bimiCertificateRemoved", name!, {});
      return changeBimi(name!, { certificate: false });
    },
  ],
  [
    "POST",
    /^\/api\/admin\/domains\/([^/]+)\/bimi\/check$/,
    (_, [name]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      found.report = report(found, found.published !== false);
      return changeBimi(name!, bimiOf(name!).enabled ? { checkedAt: Math.floor(Date.now() / 1000) } : {});
    },
  ],
];

const routes: [string, RegExp, Handler][] = [
  // First, so they win over the older routes for the same addresses.
  ...ruleRoutes,
  ...pictureMockRoutes,
  ...microsoftMockRoutes,
  ...bimiMockRoutes,
  ...assistMockRoutes,
  ["GET", /^\/api\/admin\/alerts$/, () => [200, alertsView()]],
  [
    "POST",
    /^\/api\/admin\/alerts\/(\d+)\/acknowledge$/,
    (_body, [id]) => {
      const alert = mockAlerts.find((candidate) => candidate.id === Number(id));
      if (!alert) return problem(404, "notFound");
      if (alert.resolvedAt !== null) return problem(409, "alertResolved");
      alert.acknowledgedAt = Math.floor(Date.now() / 1000);
      alert.acknowledgedBy = "lorin@uwu.example";
      log("alert.acknowledge", alert.key, { kind: alert.kind, code: alert.code });
      return [200, alert];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/stats$/,
    (_body, _match, query) => {
      const range = query.get("range") ?? "days";
      if (range !== "days" && range !== "months") return problem(422, "invalid");
      return [200, mockStats(range)];
    },
  ],
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
        if (key.endsWith("password") || key.endsWith("token") || key.endsWith("secret")) {
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
          (key.endsWith("password") || key.endsWith("token") || key.endsWith("secret")) && value !== null
            ? "•••"
            : value,
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
        // Every seventh line comes from the gateway, so its badge can be seen.
        const source = logSeq % 7 === 0 ? "gateway" : "server";
        return {
          seq: logSeq,
          at: Date.now() - (40 - logSeq) * 1000,
          source,
          level,
          target: "uwumail",
          message,
          fields,
        };
      });
      return [200, { lines, latest: logSeq }];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/logs\/loki$/,
    () => {
      const enabled = settings["log.loki.enabled"]?.value === true;
      const status: LokiStatus = {
        enabled,
        queued: enabled ? 3 : 0,
        sent: enabled ? 12_480 : 0,
        dropped: 0,
        lastSuccess: enabled ? Math.floor(Date.now() / 1000) - 4 : null,
        error: null,
      };
      return [200, status];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/logs\/loki\/test$/,
    (body) => {
      // "down" anywhere in the address plays a Loki that does not answer.
      const changes = (body as { changes: Record<string, unknown> }).changes;
      const url = String(changes["log.loki.url"] ?? settings["log.loki.url"]?.value ?? "");
      if (!/^https?:\/\//.test(url)) return problem(409, "lokiInvalid");
      if (url.includes("down")) return problem(409, "lokiUnreachable");
      return [200, { ok: true }];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/auth\/oidc\/test$/,
    (body) => {
      // "down" anywhere in the issuer plays a provider that does not answer.
      const changes = (body as { changes: Record<string, unknown> }).changes;
      const issuer = String(changes["auth.oidc.issuer"] ?? settings["auth.oidc.issuer"]?.value ?? "");
      if (!issuer.startsWith("https://")) return problem(409, "settingsInvalid");
      if (issuer.includes("down")) return problem(409, "oidcFailed");
      return [
        200,
        {
          ok: true,
          detail: `${new URL(issuer).origin} answers, with 2 signing keys`,
          redirectUri: "https://mail.uwu.example/api/auth/oidc/callback",
        },
      ];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/auth\/ldap\/test$/,
    (body) => {
      // "down" anywhere in the address plays a directory that does not answer.
      const changes = (body as { changes: Record<string, unknown> }).changes;
      const url = String(changes["auth.ldap.url"] ?? settings["auth.ldap.url"]?.value ?? "");
      if (!/^ldaps?:\/\//.test(url)) return problem(409, "settingsInvalid");
      if (url.includes("down")) return problem(409, "ldapFailed");
      const base = String(changes["auth.ldap.base_dn"] ?? settings["auth.ldap.base_dn"]?.value ?? "dc=example,dc=com");
      return [200, { ok: true, detail: `connected, and ${base} can be searched` }];
    },
  ],
  [
    "GET",
    /^\/api\/info$/,
    () => [
      200,
      {
        hostname: "mail.uwu.example",
        setupRequired: setupOpen,
        brand: brand(),
        oidc: settings["auth.oidc.enabled"]?.value
          ? { label: String(settings["auth.oidc.button_label"]?.value ?? "").trim() }
          : null,
      } satisfies Info,
    ],
  ],
  ["PUT", /^\/api\/admin\/branding\/logo$/, () => [200, brand()]],
  [
    "DELETE",
    /^\/api\/admin\/branding\/logo$/,
    () => {
      mockLogo = null;
      return [200, brand()];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/branding\/palette$/,
    (_, __, query) => {
      const color = query.get("color") ?? "";
      if (!/^#[0-9a-f]{6}$/i.test(color)) return problem(409, "brandColor");
      return [200, mockPalette(color)];
    },
  ],
  [
    "GET",
    /^\/api\/setup$/,
    () => [200, { open: setupOpen, hostname: "mail.uwu.example", domains: [] } satisfies SetupStatus],
  ],
  [
    "POST",
    /^\/api\/setup\/backup\/look$/,
    (body) => {
      const given = body as { host?: string; recoveryKey?: string };
      if (!setupOpen) return problem(409, "setupDone");
      if ((given.host ?? "").includes("wrong")) return problem(409, "backupLoginRefused");
      // A repository that wants its recovery key first: the case worth seeing.
      const snapshots = given.recoveryKey
        ? [
            {
              name: "001789900000-9f8e7d",
              createdAt: Math.floor(Date.now() / 1000) - 7200,
              hostname: "mail.old.example",
              version: "0.2.2",
              mails: 18_422,
              size: 2_310_000_000,
            },
            {
              name: "001789800000-1a2b3c",
              createdAt: Math.floor(Date.now() / 1000) - 93_600,
              hostname: "mail.old.example",
              version: "0.2.2",
              mails: 18_100,
              size: 2_290_000_000,
            },
          ]
        : [];
      return [200, { hostKey: "SHA256:uwuExampleHostKeyFingerprint0000000000000000", encrypted: true, snapshots }];
    },
  ],
  ["POST", /^\/api\/setup\/backup\/restore$/, () => (setupOpen ? [200, { started: true }] : problem(409, "setupDone"))],
  [
    "POST",
    /^\/api\/setup\/code$/,
    (body) => {
      if (!setupOpen) return problem(409, "setupDone");
      return (body as { code: string }).code.startsWith("wrong")
        ? problem(409, "setupCodeInvalid")
        : [200, { ok: true }];
    },
  ],
  [
    "POST",
    /^\/api\/setup$/,
    (body) => {
      const input = body as { domain: string; password: string };
      if (!setupOpen) return problem(409, "setupDone");
      if (input.password.length < 10) return problem(409, "weakPassword");
      const name = input.domain.toLowerCase();
      if (!domains.some((d) => d.name === name)) {
        domains.push({
          name,
          catchAll: null,
          createdAt: Math.floor(Date.now() / 1000),
          keys: [key(name, "uwu202609r", "active", "rsa-sha256"), key(name, "uwu202609e", "active", "ed25519-sha256")],
          report: null,
          published: false,
        });
      }
      setupOpen = false;
      loggedIn = true;
      log("setup.complete", name);
      return [200, session()];
    },
  ],
  ["GET", /^\/api\/admin\/setup\/check$/, () => [200, lastServerCheck]],
  ["POST", /^\/api\/admin\/setup\/reachability$/, () => [200, reachability()]],
  ["GET", /^\/api\/admin\/gateway$/, () => [200, gatewayView()]],
  [
    "POST",
    /^\/api\/admin\/gateway\/cloudflare$/,
    (body) => {
      const { token, apply, replace } = body as { token: string; apply?: boolean; replace?: boolean };
      if (token === "wrong") return problem(409, "cloudflareFailed");
      const view = gatewayView();
      if (view.addresses.length === 0) return problem(409, "gatewayNoAddresses");
      const v4 = view.addresses.filter((address) => !address.includes(":"));
      const v6 = view.addresses.filter((address) => address.includes(":"));
      // The pretend zone: the host name still points at the home connection, autoconfig is proxied.
      const plan: GatewayHostChange[] = gatewayDns
        ? [
            {
              name: "mail.uwu.example",
              recordType: "A",
              current: v4,
              wanted: v4,
              proxied: false,
              action: "none",
              note: null,
            },
            {
              name: "mail.uwu.example",
              recordType: "AAAA",
              current: v6,
              wanted: v6,
              proxied: false,
              action: "none",
              note: null,
            },
          ]
        : [
            {
              name: "mail.uwu.example",
              recordType: "A",
              current: ["198.51.100.7"],
              wanted: v4,
              proxied: false,
              action: "replace",
              note: null,
            },
            {
              name: "mail.uwu.example",
              recordType: "AAAA",
              current: [],
              wanted: v6,
              proxied: false,
              action: "create",
              note: null,
            },
            {
              name: "autoconfig.uwu.example",
              recordType: "A",
              current: v4,
              wanted: v4,
              proxied: true,
              action: "update",
              note: null,
            },
          ];
      if (!apply) return [200, { plan }];
      const results = plan
        .filter((change) => change.action !== "none")
        .map((change) => ({
          name: change.name,
          recordType: change.recordType,
          outcome: change.action === "replace" && !replace ? "skipped" : change.current.length ? "updated" : "created",
          error: null,
        }));
      if (replace) gatewayDns = true;
      log("gateway.cloudflare", "");
      return [200, { plan, results }];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/gateway\/jobs$/,
    (body) => {
      const verb = (body as { verb?: string }).verb ?? "os-update";
      if (!gateway.machine) return [409, { code: "gatewayJobRefused", detail: "the gateway is away" }];
      gatewayJobStartedAt = Date.now();
      gateway = {
        ...gateway,
        machine: {
          ...gateway.machine,
          job: { id: "mock-" + verb, state: "running", error: "", at: Math.floor(Date.now() / 1000), log: "" },
        },
      };
      return [200, gateway];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/gateway$/,
    (body) => {
      const { code = "", password } = body as { code?: string; password?: string };
      if (!code.startsWith("uwugw1") || code.includes("wrong")) {
        return problem(409, "gatewayCodeInvalid");
      }
      if (!password) return problem(409, "confirmPassword");
      gateway = {
        ...noGateway,
        state: "connecting",
        tunnel: ["203.0.113.10:443"],
        fingerprint: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        downSince: Math.floor(Date.now() / 1000),
      };
      gatewayPairedAt = Date.now();
      log("gateway.pair", "");
      return [200, gateway];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/gateway$/,
    () => {
      gateway = noGateway;
      log("gateway.forget", "");
      return [204, null];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/setup\/check$/,
    (body) => {
      lastServerCheck = serverCheck(Boolean((body as { blocklists?: boolean } | undefined)?.blocklists));
      return [200, lastServerCheck];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/setup\/test-mail$/,
    (body) => {
      const external = (body as { external?: string }).external ?? null;
      const messageId = `mock${testMails.size + 1}.test@uwu.example`;
      testMails.set(messageId, { sentAt: Date.now(), external });
      return [200, { messageId, external }];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/setup\/test-mail\/([^/]+)$/,
    (_, [id]) => {
      const sent = testMails.get(id!);
      if (!sent) return [200, { arrived: false, replyFrom: null }];
      const age = Date.now() - sent.sentAt;
      return [200, { arrived: age > 3000, replyFrom: sent.external && age > 12_000 ? sent.external : null }];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/domains\/([^/]+)\/dns\/cloudflare$/,
    (body, [name]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      if ((body as { token: string }).token === "wrong") return problem(409, "cloudflareFailed");
      const { replace = [], tidy = [] } = body as { replace?: string[]; tidy?: string[] };
      const records = (found.report ?? report(found, false)).records.filter((r) => r.recordType !== "HTTPS");
      const outcome = (r: RecordCheck) => {
        if (r.status === "missing") return "created";
        if (r.status === "wrong") return replace.includes(r.kind) ? "updated" : "skipped";
        if (r.differs) return tidy.includes(r.kind) ? "updated" : "skipped";
        // Only the DMARC record of the mock is still unquoted at the pretend Cloudflare.
        return r.kind === "dmarc" ? "requoted" : null;
      };
      found.published = true;
      log("domain.cloudflare", found.name);
      return [
        200,
        {
          results: records
            .map((r) => ({ name: r.name, recordType: r.recordType, outcome: outcome(r), error: null }))
            .filter((result) => result.outcome),
        },
      ];
    },
  ],
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
  ["GET", /^\/api\/account\/oauth-grants$/, () => [200, mockSecurity.oauthGrants]],
  [
    "DELETE",
    /^\/api\/account\/oauth-grants\/(\d+)$/,
    (_, [id]) => {
      const found = mockSecurity.oauthGrants.find((grant) => String(grant.id) === id);
      if (!found) return problem(404, "notFound");
      mockSecurity.oauthGrants = mockSecurity.oauthGrants.filter((grant) => grant !== found);
      securityEvent("oauthRevoked", { name: found.clientName });
      return [204, null];
    },
  ],
  [
    "GET",
    /^\/api\/oauth\/authorize$/,
    (_, __, query) => {
      if (!loggedIn) return problem(401, "unauthorized");
      const request = oauthRequest(Object.fromEntries(query));
      if (request[0] !== 200 || query.get("prompt") !== "none") return request;
      const answer = request[1] as OAuthRequest;
      if ("redirect" in answer || answer.consented) return request;
      return [
        200,
        { redirect: oauthAnswer(query.get("redirect_uri"), { error: "consent_required" }, query.get("state")) },
      ];
    },
  ],
  [
    "POST",
    /^\/api\/oauth\/authorize$/,
    (body) => {
      if (!loggedIn) return problem(401, "unauthorized");
      const { approve, ...fields } = body as Record<string, string> & { approve: boolean };
      const request = oauthRequest(fields);
      if (request[0] !== 200) return request;
      const answer = request[1] as OAuthRequest;
      if ("redirect" in answer) return request;
      if (!approve) {
        return [200, { redirect: oauthAnswer(fields.redirect_uri, { error: "access_denied" }, fields.state) }];
      }
      if (!answer.consented) {
        const grant = { ...oauthGrant(nextGrantId++, answer.client.name, answer.scopes, 0), createdAt: now };
        mockSecurity.oauthGrants.unshift({ ...grant, lastUsedAt: null, lastUsedProtocol: null, lastUsedIp: null });
        securityEvent("oauthGranted", { name: answer.client.name, scopes: answer.scopes });
      }
      return [200, { redirect: oauthAnswer(fields.redirect_uri, { code: "mock-code" }, fields.state) }];
    },
  ],
  [
    "GET",
    /^\/api\/account\/spam$/,
    () => [
      200,
      {
        limits: mockLimits,
        bayes: { enabled: true, minimum: 50, own: mockBayes.own, server: mockBayes.server },
      } satisfies AccountSpamView,
    ],
  ],
  [
    "PUT",
    /^\/api\/account\/spam\/limits$/,
    (body) => {
      const own = body as SpamLimits;
      if (own.junk !== null && own.reject !== null && own.reject < own.junk) return problem(409, "spamLimitsOrder");
      mockLimits.own = own;
      return [200, mockLimits];
    },
  ],
  [
    "GET",
    /^\/api\/account\/greylist$/,
    () => [200, { enabled: true, waiting: mockGreylist, count: mockGreylist.length } satisfies GreylistView],
  ],
  [
    "POST",
    /^\/api\/account\/greylist\/(\d+)$/,
    (_, [id]) => {
      const at = mockGreylist.findIndex((hold) => hold.id === Number(id));
      if (at < 0) return problem(404, "notFound");
      mockGreylist.splice(at, 1);
      return [200, { enabled: true, waiting: mockGreylist, count: mockGreylist.length } satisfies GreylistView];
    },
  ],
  ["GET", /^\/api\/account\/spam\/senders$/, () => [200, sendersView("own")]],
  ["POST", /^\/api\/account\/spam\/senders$/, (body) => addSender("own", body)],
  ["DELETE", /^\/api\/account\/spam\/senders\/(\d+)$/, (_, [id]) => removeSender("own", id)],
  [
    "GET",
    /^\/api\/admin\/spam\/log$/,
    () => [
      200,
      {
        entries: spamLogEntries,
        total: spamLogEntries.length,
        oldest: now - 3 * 86_400,
        settings: { enabled: true, cleanSubjects: false, retentionDays: 30 },
        maxRows: 200_000,
      } satisfies SpamLogView,
    ],
  ],
  ["DELETE", /^\/api\/admin\/spam\/log$/, () => [200, { removed: spamLogEntries.length }]],
  [
    "GET",
    /^\/api\/admin\/spam\/antivirus$/,
    () => {
      const enabled = Boolean(settings["spam.antivirus.enabled"]?.value);
      return [
        200,
        {
          enabled,
          address: String(settings["spam.antivirus.address"]?.value ?? ""),
          maxSize: Number(settings["spam.antivirus.max_size"]?.value ?? 0),
          status: enabled
            ? { version: "ClamAV 1.5.4/27700/Wed Sep 17 08:32:11 2026", signatures: 27_700, signaturesAt: now - 3600 }
            : null,
          signaturesOld: false,
          error: null,
          days: 30,
          found: 2,
        } satisfies AntivirusView,
      ];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/spam\/antivirus\/test$/,
    () => {
      if (!settings["spam.antivirus.enabled"]?.value) return problem(409, "virusScannerOff");
      log("spam.virusTest", "server");
      return [200, { found: "Eicar-Test-Signature", error: null } satisfies AntivirusTest];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/egress$/,
    () => [
      200,
      {
        proxy: "http://gluetun:8888",
        fallback: "block",
        fetched: 1284,
        failed: 17,
        proxyFailures: 3,
        fallbacks: 0,
        lastProxyFailure: { at: now - 5 * 3600, error: "the proxy did not answer in time" },
      } satisfies EgressView,
    ],
  ],
  [
    "POST",
    /^\/api\/admin\/egress\/test$/,
    () => {
      log("egress.test", "server");
      return [200, { proxied: true, address: "185.107.56.10", error: null } satisfies EgressTest];
    },
  ],
  ["GET", /^\/api\/admin\/host$/, () => [200, hostView()]],
  [
    "POST",
    /^\/api\/admin\/host\/jobs$/,
    (body) => {
      const { verb } = body as { verb: string };
      hostJob = { verb, asked: Date.now() };
      log("host.job", verb);
      return [200, hostView()];
    },
  ],
  ["GET", /^\/api\/admin\/spam\/senders$/, () => [200, sendersView("admin")]],
  ["POST", /^\/api\/admin\/spam\/senders$/, (body) => addSender("admin", body)],
  ["DELETE", /^\/api\/admin\/spam\/senders\/(\d+)$/, (_, [id]) => removeSender("admin", id)],
  ["GET", /^\/api\/account\/spam\/words$/, () => [200, mockWords.own]],
  ["POST", /^\/api\/account\/spam\/words$/, (body) => addWords("own", body)],
  [
    "DELETE",
    /^\/api\/account\/spam\/words\/(\d+)$/,
    (_, [id]) => {
      mockWords.own.entries = mockWords.own.entries.filter((entry) => String(entry.id) !== id);
      return [200, mockWords.own];
    },
  ],
  ["POST", /^\/api\/account\/spam\/word-sources$/, (body) => subscribeWords("own", body)],
  [
    "DELETE",
    /^\/api\/account\/spam\/word-sources\/(\d+)$/,
    (_, [id]) => {
      mockWords.own.sources = mockWords.own.sources.filter((source) => String(source.id) !== id);
      return [200, mockWords.own];
    },
  ],
  ["POST", /^\/api\/account\/spam\/word-sources\/\d+\/refresh$/, () => [200, { error: null, lists: mockWords.own }]],
  ["GET", /^\/api\/admin\/spam\/words$/, () => [200, mockWords.admin]],
  ["POST", /^\/api\/admin\/spam\/words$/, (body) => addWords("admin", body)],
  [
    "DELETE",
    /^\/api\/admin\/spam\/words\/(\d+)$/,
    (_, [id]) => {
      mockWords.admin.entries = mockWords.admin.entries.filter((entry) => String(entry.id) !== id);
      return [200, mockWords.admin];
    },
  ],
  ["POST", /^\/api\/admin\/spam\/word-sources$/, (body) => subscribeWords("admin", body)],
  [
    "DELETE",
    /^\/api\/admin\/spam\/word-sources\/(\d+)$/,
    (_, [id]) => {
      mockWords.admin.sources = mockWords.admin.sources.filter((source) => String(source.id) !== id);
      return [200, mockWords.admin];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/spam\/word-sources\/\d+\/refresh$/,
    () => [200, { error: "the link answered 404 Not Found", lists: mockWords.admin }],
  ],
  ["GET", /^\/api\/admin\/spam\/feeds$/, () => [200, mockFeeds]],
  [
    "POST",
    /^\/api\/admin\/spam\/feeds\/(\w+)\/refresh$/,
    (_, [key]) => {
      const feed = mockFeeds.feeds.find((entry) => entry.key === key);
      if (!feed?.active) return problem(409, "feedInactive");
      feed.fetchedAt = now;
      feed.error = null;
      return [200, { ...mockFeeds, error: null }];
    },
  ],
  ["POST", /^\/api\/account\/spam\/learn-folders$/, () => [200, { spam: 12, ham: 87 } satisfies LearnedFromFolders]],
  [
    "GET",
    /^\/api\/admin\/spam$/,
    () => [
      200,
      {
        bayes: { enabled: true, minimum: 50, server: mockBayes.server, queued: 3 },
        fetched: {
          since: now - 30 * 86_400,
          total: 412,
          agreedJunk: 288,
          weLetThrough: 19,
          weCaught: 37,
          agreedClean: 68,
        },
        fetchedDays: 30,
      } satisfies AdminSpamView,
    ],
  ],
  [
    "POST",
    /^\/api\/admin\/spam\/learn-folders$/,
    () => [200, { spam: 40, ham: 310, people: 3 } satisfies LearnedFromFolders],
  ],
  ["GET", /^\/api\/account\/moving$/, () => [200, stepMoves()]],
  [
    "POST",
    /^\/api\/account\/moving$/,
    (body) => {
      const given = body as { address: string; password: string; host?: string; login?: string };
      const address = given.address.trim().toLowerCase();
      if (!address.includes("@")) return problem(409, "senderInvalid");
      if (address.endsWith("@uwu.example")) return problem(409, "moveFromHere");
      if (given.password === "wrong") return problem(409, "moveWrongPassword");
      if (address.endsWith("@unknown.example") && !given.host) return problem(409, "providerNotFound");
      if (mockMoves.some((job) => job.address === address)) return problem(409, "moveExists");
      if (mockMoves.length >= 5) return problem(409, "moveLimit");
      const job: MoveJob = {
        id: nextMoveId++,
        address,
        host: given.host?.trim() || `imap.${address.split("@")[1]}`,
        port: 993,
        login: given.login?.trim() || address,
        state: "queued",
        error: "",
        errorDetail: "",
        foldersDone: 0,
        foldersTotal: 0,
        messagesDone: 0,
        messagesTotal: 0,
        messagesSkipped: 0,
        bytesDone: 0,
        createdAt: Math.floor(Date.now() / 1000),
        startedAt: null,
        finishedAt: null,
        lastRunAt: null,
      };
      mockMoves.unshift(job);
      moveStartedAt.set(job.id, Date.now());
      return [201, job];
    },
  ],
  [
    "POST",
    /^\/api\/account\/moving\/(\d+)\/sync$/,
    (body, [id]) => {
      const job = mockMoves.find((candidate) => candidate.id === Number(id));
      if (!job) return problem(404, "notFound");
      if (job.state === "queued" || job.state === "running") return problem(409, "moveRunning");
      if (job.state === "done") {
        Object.assign(job, { foldersDone: 0, messagesDone: 0, messagesTotal: 0, messagesSkipped: 0, bytesDone: 0 });
        Object.assign(job, { startedAt: null, finishedAt: null });
      }
      void body;
      Object.assign(job, { state: "queued", error: "", errorDetail: "" });
      moveStartedAt.set(job.id, Date.now());
      return [200, job];
    },
  ],
  [
    "POST",
    /^\/api\/account\/moving\/(\d+)\/pause$/,
    (_, [id]) => {
      const job = mockMoves.find((candidate) => candidate.id === Number(id));
      if (!job) return problem(404, "notFound");
      if (job.state !== "queued" && job.state !== "running") return problem(409, "moveNotRunning");
      Object.assign(job, { state: "paused", error: "stopped" });
      return [200, job];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/moving\/(\d+)$/,
    (_, [id]) => {
      const at = mockMoves.findIndex((candidate) => candidate.id === Number(id));
      if (at < 0) return problem(404, "notFound");
      mockMoves.splice(at, 1);
      return [204, null];
    },
  ],
  ["GET", /^\/api\/account\/fetch$/, () => [200, mockFetchView()]],
  [
    "POST",
    /^\/api\/account\/fetch\/provider$/,
    (body) => {
      const provider = mockProviderOf(((body as { address?: string }).address ?? "").trim());
      return [200, { provider, ready: provider !== null }];
    },
  ],
  [
    "POST",
    /^\/api\/account\/fetch\/oauth\/start$/,
    (body) => {
      const input = body as { address?: string; provider?: "microsoft" | "google"; switchId?: number };
      const address = (input.address ?? "").trim().toLowerCase();
      if (!address.includes("@")) return problem(409, "senderInvalid");
      const flowId = `mockflow${Math.random().toString(36).slice(2, 12)}`;
      const provider = input.provider === "google" ? "google" : "microsoft";
      mockSignIns.set(flowId, { provider, address, switchId: input.switchId ?? null, polls: 0 });
      if (provider === "google") {
        // Google would come back to the callback, which sends the browser here.
        return [200, { provider, flowId, url: `/account/fetch?oauth=${flowId}` }];
      }
      return [
        200,
        {
          provider,
          device: {
            flowId,
            userCode: "KX7-PQ4M",
            verificationUri: "https://microsoft.com/devicelogin",
            expiresIn: 900,
            interval: 2,
          },
        },
      ];
    },
  ],
  [
    "GET",
    /^\/api\/account\/fetch\/oauth\/flows\/([\w-]+)$/,
    (_body, match) => {
      const id = match[0] ?? "";
      // Google's way round reloads the page, and the mock with it: a mock flow comes back as a new
      // Google mailbox then.
      if (!mockSignIns.has(id) && id.startsWith("mockflow")) {
        mockSignIns.set(id, { provider: "google", address: "neu.mock@gmail.com", switchId: null, polls: 2 });
      }
      const flow = mockSignIns.get(id);
      if (!flow) return [200, { status: "failed", error: "expired" }];
      flow.polls += 1;
      if (flow.polls < 4) return [200, { status: "pending", retryIn: 2 }];
      const microsoft = flow.provider === "microsoft";
      return [
        200,
        {
          status: "ready",
          provider: flow.provider,
          address: flow.address,
          switchId: flow.switchId,
          settings: {
            imap: {
              host: microsoft ? "outlook.office365.com" : "imap.gmail.com",
              port: 993,
              security: "tls",
              login: "wholeAddress",
            },
            smtp: {
              host: microsoft ? "smtp-mail.outlook.com" : "smtp.gmail.com",
              port: microsoft ? 587 : 465,
              security: microsoft ? "starttls" : "tls",
              login: "wholeAddress",
            },
            source: "signIn",
          },
        },
      ];
    },
  ],
  [
    // The real one asks DNS, the provider and Mozilla and then logs in; here two addresses stand
    // for the three ways it can go, so the page can be worked on without a provider.
    "POST",
    /^\/api\/account\/fetch\/discover$/,
    (body) => {
      const input = body as { address?: string; password?: string };
      const address = (input.address ?? "").trim().toLowerCase();
      const domain = address.split("@")[1] ?? "";
      if (!domain.includes(".")) return problem(409, "senderInvalid");
      if (!input.password) return problem(409, "wrongPassword");
      // Microsoft's "Basic authentication is disabled": the dialog switches to signing in.
      if (mockProviderOf(address) === "microsoft") return problem(409, "passwordsRefused");
      // Whatever nobody publishes anything for: the dialog opens its fields.
      if (domain.endsWith("nowhere.example")) return problem(409, "providerNotFound");
      const localPart = ["icloud.com", "me.com", "web.de"].includes(domain);
      return [
        200,
        {
          imap: {
            host: domain === "icloud.com" ? "imap.mail.me.com" : `imap.${domain}`,
            port: 993,
            security: "tls",
            login: localPart ? "localPart" : "wholeAddress",
          },
          smtp: {
            host: domain === "icloud.com" ? "smtp.mail.me.com" : `smtp.${domain}`,
            port: 587,
            security: "starttls",
            login: "wholeAddress",
          },
          source: domain === "icloud.com" ? "domain" : "database",
        },
      ];
    },
  ],
  [
    "POST",
    /^\/api\/account\/fetch$/,
    (body) => {
      const input = body as Partial<FetchAccountInfo> & {
        password?: string;
        takeExisting?: boolean;
        oauthFlow?: string;
      };
      const address = (input.address ?? "").trim().toLowerCase();
      if (mockFetchAccounts.some((account) => account.address === address)) return problem(409, "conflict");
      const signIn = input.oauthFlow ? mockSignIns.get(input.oauthFlow) : undefined;
      if (input.oauthFlow && (!signIn || signIn.address !== address)) return problem(409, "signInExpired");
      if (input.oauthFlow) mockSignIns.delete(input.oauthFlow);
      const microsoft = signIn?.provider === "microsoft";
      const account: FetchAccountInfo = {
        id: mockFetchAccounts.length + 1,
        accountId: 1,
        address,
        host: signIn ? (microsoft ? "outlook.office365.com" : "imap.gmail.com") : (input.host ?? ""),
        port: input.port ?? 993,
        security: "tls",
        username: input.username ?? address,
        afterFetch: input.afterFetch ?? "markRead",
        fetchJunk: input.fetchJunk ?? true,
        intervalSecs: input.intervalSecs ?? 300,
        enabled: true,
        authServId: "",
        smtpHost: input.smtpHost ?? "",
        smtpPort: input.smtpPort ?? 587,
        smtpSecurity: input.smtpSecurity ?? "starttls",
        sendEnabled: input.sendEnabled ?? false,
        createdAt: Math.floor(Date.now() / 1000),
        lastRunAt: null,
        lastOkAt: null,
        lastError: "",
        lastFetched: 0,
        totalFetched: 0,
        backlogAt: input.takeExisting ? Math.floor(Date.now() / 1000) : null,
        auth: signIn ? signIn.provider : "password",
        loginExpired: false,
        passwordRefused: false,
        signIn: signIn ? null : mockProviderOf(address),
      };
      if (signIn) {
        account.smtpHost = microsoft ? "smtp-mail.outlook.com" : "smtp.gmail.com";
        account.smtpPort = microsoft ? 587 : 465;
        account.smtpSecurity = microsoft ? "starttls" : "tls";
      }
      mockFetchAccounts.push(account);
      return [201, account];
    },
  ],
  [
    // The real one only asks for it and lets the next runs do the work; here nothing runs, so it
    // stays "coming over" -- which is the state the row has to show anyway.
    "POST",
    /^\/api\/account\/fetch\/(\d+)\/existing$/,
    (_body, match) => {
      const account = mockFetchAccounts.find((entry) => entry.id === Number(match[0]));
      if (!account) return problem(404, "notFound");
      account.backlogAt = Math.floor(Date.now() / 1000);
      return [202, null];
    },
  ],
  [
    "PATCH",
    /^\/api\/account\/fetch\/(\d+)$/,
    (body, match) => {
      const account = mockFetchAccounts.find((entry) => entry.id === Number(match[0]));
      if (!account) return problem(404, "notFound");
      const changes = body as Partial<FetchAccountInfo> & { oauthFlow?: string; password?: string };
      if (changes.oauthFlow) {
        const signIn = mockSignIns.get(changes.oauthFlow);
        if (!signIn || signIn.switchId !== account.id) return problem(409, "signInExpired");
        mockSignIns.delete(changes.oauthFlow);
        Object.assign(account, {
          auth: signIn.provider,
          loginExpired: false,
          passwordRefused: false,
          signIn: null,
          lastError: "",
        });
        return [200, account];
      }
      if (changes.password) {
        Object.assign(account, { auth: "password", loginExpired: false, passwordRefused: false });
        delete changes.password;
      }
      // The server refuses both of these, and a mock that is friendlier than the server hides
      // exactly the mistakes this page is written to avoid.
      if (changes.sendEnabled) {
        const host = changes.smtpHost ?? account.smtpHost;
        if (!host.trim()) return problem(409, "invalid");
        if (account.lastOkAt === null) return problem(409, "invalid");
      }
      Object.assign(account, changes);
      return [200, account];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/fetch\/(\d+)$/,
    (_body, match) => {
      const at = mockFetchAccounts.findIndex((entry) => entry.id === Number(match[0]));
      if (at < 0) return problem(404, "notFound");
      mockFetchAccounts.splice(at, 1);
      return [204, null];
    },
  ],
  ["POST", /^\/api\/account\/fetch\/(\d+)\/run$/, () => [202, null]],
  ["GET", /^\/api\/account\/forwarding$/, () => [200, mockForwarding]],
  [
    "POST",
    /^\/api\/account\/forwarding\/targets$/,
    (body) => {
      const address = (body as { address: string }).address.trim().toLowerCase();
      if (address === "lorin@uwu.example") return problem(409, "forwardToSelf");
      if (mockForwarding.targets.some((target) => target.address === address)) return problem(409, "conflict");
      const local = address.endsWith("@uwu.example") || address.endsWith("@verein.example");
      const at = Math.floor(Date.now() / 1000);
      mockForwarding.targets.push({
        id: nextSecurityId++,
        address,
        local,
        createdAt: at,
        confirmedAt: local ? at : null,
      });
      securityEvent("forwardingAdded", { address });
      return [201, mockForwarding];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/forwarding\/targets\/(\d+)$/,
    (_, [id]) => {
      mockForwarding.targets = mockForwarding.targets.filter((target) => String(target.id) !== id);
      return [200, mockForwarding];
    },
  ],
  [
    "PUT",
    /^\/api\/account\/forwarding\/keep-copy$/,
    (body) => {
      mockForwarding.keepCopy = (body as { keep: boolean }).keep;
      return [200, mockForwarding];
    },
  ],
  ["GET", /^\/api\/account\/vacation$/, () => [200, mockVacation]],
  ["GET", /^\/api\/account\/identities$/, () => [200, mockIdentities]],
  ["GET", /^\/api\/account\/signatures$/, () => [200, mockSignatureOverview()]],
  ["PUT", /^\/api\/account\/signatures$/, (body) => [200, mockChangeSignatures(body as SignatureChange)]],
  [
    "PATCH",
    /^\/api\/account\/identities\/(\d+)$/,
    (body, match) => {
      const identity = mockIdentities.find((entry) => entry.id === Number(match[0]));
      if (!identity) return problem(404, "notFound");
      Object.assign(identity, body as Partial<IdentityInfo>);
      return [204, null];
    },
  ],
  ["GET", /^\/api\/account\/addresses$/, () => [200, mockAddresses]],
  ["GET", /^\/api\/account\/masked$/, () => [200, maskedView()]],
  [
    "POST",
    /^\/api\/account\/masked$/,
    (body) => {
      const input = body as { domain?: string; description: string; forDomain: string; emailPrefix: string | null };
      const view = maskedView();
      const domain = input.domain || view.defaultDomain;
      if (!domain || !view.domains.includes(domain)) return problem(409, "maskedDomain");
      if (input.emailPrefix && !/^[a-z0-9_]{1,64}$/i.test(input.emailPrefix)) return problem(409, "maskedPrefix");
      const word = () => MASKED_WORDS[Math.floor(Math.random() * MASKED_WORDS.length)]!;
      const random = `${word()}.${word()}${String(Math.floor(Math.random() * 1000)).padStart(3, "0")}`;
      const local = input.emailPrefix ? `${input.emailPrefix.toLowerCase()}.${random}` : random;
      const created: MaskedAddress = {
        id: Math.max(0, ...mockMasked.map((entry) => entry.id)) + 1,
        email: `${local}@${domain}`,
        state: "enabled",
        forDomain: input.forDomain,
        description: input.description,
        url: null,
        emailPrefix: input.emailPrefix,
        createdBy: "Portal",
        createdAt: Math.floor(Date.now() / 1000),
        lastMessageAt: null,
      };
      mockMasked.unshift(created);
      return [201, created];
    },
  ],
  [
    "PATCH",
    /^\/api\/account\/masked\/(\d+)$/,
    (body, [id]) => {
      const found = mockMasked.find((entry) => entry.id === Number(id));
      if (!found) return problem(404, "notFound");
      const changes = body as { state?: MaskedState; description?: string; forDomain?: string };
      if (changes.state === "pending") return problem(422, "invalid");
      Object.assign(found, changes);
      return [200, found];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/masked\/(\d+)$/,
    (_, [id]) => {
      const found = mockMasked.find((entry) => entry.id === Number(id));
      if (!found) return problem(404, "notFound");
      found.state = "deleted";
      return [200, maskedView()];
    },
  ],
  [
    "POST",
    /^\/api\/account\/aliases$/,
    (body) => {
      const address = (body as { address: string }).address.trim().toLowerCase();
      if (address.startsWith("postmaster@")) return problem(409, "aliasReserved");
      if (mockAddresses.addresses.some((entry) => entry.address === address)) return problem(409, "addressTaken");
      if (mockAddresses.used >= mockAddresses.limit) return problem(409, "aliasLimit");
      mockAddresses.addresses.push({ address, kind: "alias", own: true, createdAt: Math.floor(Date.now() / 1000) });
      mockAddresses.released = mockAddresses.released.filter((entry) => entry.address !== address);
      refreshAddresses();
      return [201, mockAddresses];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/aliases\/([^/]+)$/,
    (_, [address]) => {
      const at = Math.floor(Date.now() / 1000);
      mockAddresses.addresses = mockAddresses.addresses.filter((entry) => entry.address !== address);
      mockAddresses.released.unshift({ address: address ?? "", releasedAt: at, reservedUntil: at + 30 * 86_400 });
      refreshAddresses();
      return [200, mockAddresses];
    },
  ],
  ["GET", /^\/api\/account\/storage$/, () => [200, mockStorage]],
  ["GET", /^\/api\/account\/sharing$/, () => [200, mockSharing]],
  [
    "PUT",
    /^\/api\/account\/sharing\/(\d+)$/,
    (body, [id]) => {
      const { login, level } = body as { login: string; level: ShareLevel };
      const folder = mockSharing.folders.find((entry) => entry.id === Number(id));
      const person = mockSharing.people.find((entry) => entry.login === login);
      if (!folder || !person) return problem(404, "notFound");
      const rights = { read: "lr", write: "lrswite", all: "lrswipkxtea" }[level];
      folder.shares = [
        ...folder.shares.filter((entry) => entry.login !== login),
        { login, name: person.name, level, rights },
      ];
      return [200, mockSharing];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/sharing\/(\d+)\/([^/]+)$/,
    (_, [id, login]) => {
      const folder = mockSharing.folders.find((entry) => entry.id === Number(id));
      if (folder) folder.shares = folder.shares.filter((entry) => entry.login !== login);
      return [200, mockSharing];
    },
  ],
  ["GET", /^\/api\/account\/calendars$/, () => [200, mockCalendars]],
  [
    "PUT",
    /^\/api\/account\/calendars\/(\d+)\/shares$/,
    (body, [id]) => {
      const { address, rights } = body as { address: string; rights: ShareRights };
      const collection = mockCalendars.own.find((entry) => entry.id === Number(id));
      if (!collection) return problem(404, "notFound");
      const wanted = address.trim().toLowerCase();
      if (wanted === session().account.login) return problem(409, "ownShare");
      const person = mockPeople.find((entry) => entry.address === wanted);
      if (!person) return problem(409, "unknownPerson");
      collection.shares = [
        ...collection.shares.filter((entry) => entry.accountId !== person.accountId),
        { ...person, rights },
      ];
      return [200, mockCalendars];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/calendars\/(\d+)\/shares\/(\d+)$/,
    (_, [id, account]) => {
      const collection = mockCalendars.own.find((entry) => entry.id === Number(id));
      if (collection) collection.shares = collection.shares.filter((entry) => entry.accountId !== Number(account));
      return [200, mockCalendars];
    },
  ],
  [
    "POST",
    /^\/api\/account\/calendars\/import$/,
    (_, __, query) => {
      const kind = query?.get("kind") === "addressbook" ? "addressbook" : "calendar";
      const target = query?.get("target");
      const imported = mockImport(
        kind,
        query?.get("name") || query?.get("fileName") || "Import",
        target ? Number(target) : undefined,
      );
      return [200, { ...mockCalendars, ...imported }];
    },
  ],
  [
    "POST",
    /^\/api\/account\/calendars\/import-url$/,
    (body) => {
      const { url, name } = body as { url: string; name?: string };
      if (!/^(https|webcals?):\/\//i.test(url.trim())) return problem(409, "urlNotAllowed");
      return [200, { ...mockCalendars, ...mockImport("calendar", name || "Kalender") }];
    },
  ],
  [
    "POST",
    /^\/api\/account\/calendar-subscriptions$/,
    (body) => {
      const { url, name, intervalSecs } = body as { url: string; name?: string; intervalSecs?: number };
      if (!/^(https|webcals?):\/\//i.test(url.trim())) return problem(409, "urlNotAllowed");
      const now = Math.floor(Date.now() / 1000);
      const id = ++mockCollectionId;
      const source = `${url.replace(/^[a-z]+:\/\//i, "").split("/")[0]}/…`;
      mockCalendars.own.push({
        id,
        kind: "calendar",
        name: name || "Abo",
        color: "#F6BF26FF",
        entries: 8,
        shares: [],
        isDefault: false,
        subscription: {
          id,
          collectionId: id,
          source,
          intervalSecs: intervalSecs ?? 3600,
          keepAlarms: false,
          enabled: true,
          nextRunAt: now + 3600,
          lastRunAt: now,
          lastOkAt: now,
          lastError: "",
          failures: 0,
          entries: 8,
          createdAt: now,
        },
      });
      const report = { created: 8, updated: 0, deleted: 0, unchanged: 0, problems: [], entries: 8 };
      return [200, { ...mockCalendars, report, collection: { id, kind: "calendar", name: name || "Abo" } }];
    },
  ],
  [
    "PATCH",
    /^\/api\/account\/calendar-subscriptions\/(\d+)$/,
    (body, [id]) => {
      const collection = mockCalendars.own.find((entry) => entry.subscription?.id === Number(id));
      if (!collection?.subscription) return problem(404, "notFound");
      const change = body as { intervalSecs?: number; enabled?: boolean; name?: string };
      if (change.intervalSecs) collection.subscription.intervalSecs = change.intervalSecs;
      if (change.enabled !== undefined) collection.subscription.enabled = change.enabled;
      if (change.name) collection.name = change.name;
      return [200, mockCalendars];
    },
  ],
  [
    "POST",
    /^\/api\/account\/calendar-subscriptions\/(\d+)\/refresh$/,
    (_, [id]) => {
      const collection = mockCalendars.own.find((entry) => entry.subscription?.id === Number(id));
      if (!collection?.subscription) return problem(404, "notFound");
      const now = Math.floor(Date.now() / 1000);
      if (collection.subscription.lastRunAt && now - collection.subscription.lastRunAt < 60) {
        return problem(409, "refreshPause");
      }
      collection.subscription.lastRunAt = now;
      collection.subscription.lastOkAt = now;
      return [200, mockCalendars];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/calendar-subscriptions\/(\d+)$/,
    (_, [id], query) => {
      const collection = mockCalendars.own.find((entry) => entry.subscription?.id === Number(id));
      if (!collection) return problem(404, "notFound");
      if (query?.get("keep") === "true") collection.subscription = null;
      else mockCalendars.own = mockCalendars.own.filter((entry) => entry !== collection);
      return [200, mockCalendars];
    },
  ],
  [
    "POST",
    /^\/api\/account\/calendars\/remote$/,
    (body) => {
      const { address, password, kinds } = body as { address: string; password: string; kinds: string[] };
      const domain = address.split("@")[1] ?? "";
      if (["gmail.com", "googlemail.com"].includes(domain)) return problem(409, "googleUseIcs");
      if (password === "falsch") return problem(409, "wrongPassword");
      const results = [];
      if (kinds.includes("calendar"))
        results.push({ kind: "calendar", name: "Privat", ...mockImport("calendar", "Privat") });
      if (kinds.includes("addressbook")) {
        results.push({ kind: "addressbook", name: "Kontakte", ...mockImport("addressbook", "Kontakte (alt)") });
      }
      return [200, { ...mockCalendars, provider: domain === "icloud.com" ? "iCloud" : null, results }];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/shared-calendars\/(\d+)$/,
    (_, [id]) => {
      mockCalendars.shared = mockCalendars.shared.filter((entry) => entry.id !== Number(id));
      return [200, mockCalendars];
    },
  ],
  [
    "POST",
    /^\/api\/account\/mailboxes\/(trash|junk)\/empty$/,
    (_, [role]) => {
      const mailbox = mockStorage.mailboxes.find((entry) => entry.role === role);
      const removed = mailbox?.emails ?? 0;
      if (mailbox) {
        mockStorage.usedBytes -= mailbox.sizeBytes;
        mailbox.emails = 0;
        mailbox.sizeBytes = 0;
      }
      return [200, { removed }];
    },
  ],
  [
    "PUT",
    /^\/api\/account\/vacation$/,
    (body) => {
      const next = body as VacationView;
      if (next.isEnabled && !next.textBody?.trim()) return problem(409, "vacationText");
      mockVacation = next;
      return [200, mockVacation];
    },
  ],
  [
    "GET",
    /^\/api\/forwarding-links\/([^/]+)$/,
    (_, [token]) =>
      token === "expired"
        ? problem(409, "linkInvalid")
        : [200, { address: "oma@elsewhere.example", from: "lorin@uwu.example", name: "Lorin" }],
  ],
  [
    "POST",
    /^\/api\/forwarding-links\/([^/]+)\/(confirm|decline)$/,
    () => [200, { address: "oma@elsewhere.example", from: "lorin@uwu.example" }],
  ],
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
    "POST",
    /^\/api\/account\/apple-profiles$/,
    (body) => {
      const { device } = body as { device: string };
      const appPassword: AppPasswordInfo = {
        id: nextSecurityId++,
        name: device.trim(),
        scopes: ["mail", "smtp"],
        createdAt: Math.floor(Date.now() / 1000),
        expiresAt: null,
        lastUsedAt: null,
        lastUsedProtocol: null,
        lastUsedIp: null,
      };
      mockSecurity.appPasswords.unshift(appPassword);
      securityEvent("appPasswordCreated", { name: appPassword.name });
      // The demo has no profile to download; staying on the page is enough.
      return [201, { appPassword, url: "#profile-downloaded" }];
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
        // More than the card shows at once, so the way to the rest is there to be tried.
        addresses: [
          "lorin@uwu.example",
          "hallo@uwu.example",
          "nyu@uwu.example",
          "post@uwu.example",
          "shop@uwu.example",
          "verein@uwu.example",
        ],
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
      const input = body as { name: string; kind?: DomainKind };
      const name = input.name.toLowerCase();
      if (domains.some((d) => d.name === name)) return problem(409, "conflict");
      const created: MockDomain = {
        name,
        kind: input.kind ?? "mail",
        catchAll: null,
        createdAt: Math.floor(Date.now() / 1000),
        keys: [key(name, "uwu202609r", "active", "rsa-sha256"), key(name, "uwu202609e", "active", "ed25519-sha256")],
        report: null,
      };
      domains.push(created);
      domains.sort((a, b) => a.name.localeCompare(b.name));
      log("domain.create", name, { kind: created.kind });
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
      const found = domains[index]!;
      const inUse =
        addressCount(name!, "primary") +
        addressCount(name!, "alias") +
        (found.forwards?.length ?? 0) +
        (found.groups?.length ?? 0) +
        detail(found).maskedInUse!;
      if (inUse > 0) return problem(409, "domainInUse");
      const usedBy = maskedUsedBy(name!);
      for (const other of domains) {
        if (!other.maskedPolicy) continue;
        other.maskedPolicy.maskedDomains = other.maskedPolicy.maskedDomains.filter((entry) => entry !== name);
        if (other.maskedPolicy.defaultDomain === name) other.maskedPolicy.defaultDomain = null;
      }
      for (const custom of Object.values(mockMaskedCustom)) {
        if (custom.maskedDomains) custom.maskedDomains = custom.maskedDomains.filter((entry) => entry !== name);
        if (custom.defaultDomain === name) custom.defaultDomain = null;
      }
      domains.splice(index, 1);
      log("domain.remove", name!, { removedFromDomains: usedBy.domains, removedFromAccounts: usedBy.accounts });
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
      found.report = report(found, found.published !== false && (!pending || rotationChecks > 1));
      return [200, found.report];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/mta-sts$/,
    (body, [name]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      const mode = (body as { mode: "off" | "testing" | "enforce" }).mode;
      // Log in as the mock admin and pick enforce on verein.example to see the certificate error.
      if (mode === "enforce" && found.name === "verein.example") return problem(409, "mtaStsCertificate");
      found.mtaSts = mode === "off" ? null : mtaStsView(mode, Math.floor(Date.now() / 1000));
      found.report = report(found, found.name !== "verein.example");
      log("domain.mtaSts", found.name, { mode });
      return [200, detail(found)];
    },
  ],
  ["GET", /^\/api\/admin\/domains\/([^/]+)\/reports$/, (_, [name]) => [200, reportsFor(name ?? "")]],
  [
    "GET",
    /^\/api\/admin\/domains\/([^/]+)\/reports\/(dmarc|tls)$/,
    (_, [name, kind]) => [200, { reports: name === "uwu.example" ? singleReports(kind as ReportKind) : [] }],
  ],
  [
    "GET",
    /^\/api\/admin\/domains\/([^/]+)\/reports\/(dmarc|tls)\/(\d+)$/,
    (_, [, kind, id]) => {
      const entry = singleReports(kind as ReportKind).find((report) => report.id === Number(id));
      if (!entry) return problem(404, "notFound");
      if (kind === "tls") {
        return [
          200,
          {
            kind: "tls",
            report: entry,
            policy: "version: STSv1\nmode: enforce\nmx: mail.uwu.example\nmax_age: 604800",
            failures: [
              {
                policyType: "sts",
                resultType: "certificate-expired",
                mxHost: "mail.uwu.example",
                sendingIp: "2001:db8:abcd:12::1",
                sessions: 2,
                failureCode: "certificate has expired",
                receivingIp: "192.0.2.10",
                helo: "mail.uwu.example",
                detail: null,
              },
            ],
          } satisfies ReportDetail,
        ];
      }
      return [
        200,
        {
          kind: "dmarc",
          report: entry,
          rows: [
            {
              sourceIp: "192.0.2.10",
              messages: 1198,
              dkimAligned: true,
              spfAligned: true,
              disposition: "none",
              headerFrom: "uwu.example",
              dkimDomain: "uwu.example",
              dkimSelector: "uwu202609e",
              dkimResult: "pass",
              spfDomain: "uwu.example",
              spfResult: "pass",
              overrideReason: null,
              envelopeFrom: "nyu@uwu.example",
              envelopeTo: null,
              ours: true,
            },
            {
              sourceIp: "198.51.100.77",
              messages: 39,
              dkimAligned: false,
              spfAligned: false,
              disposition: "quarantine",
              headerFrom: "uwu.example",
              dkimDomain: null,
              dkimSelector: null,
              dkimResult: "none",
              spfDomain: "spammer.example",
              spfResult: "pass",
              overrideReason: null,
              envelopeFrom: "bounce@spammer.example",
              envelopeTo: null,
              ours: false,
            },
          ],
        } satisfies ReportDetail,
      ];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/reports\/sent$/,
    () => [
      200,
      {
        days: 30,
        sender: "noreply-tls-reports@uwu.example",
        reports: [
          {
            day: Math.floor(now / 86_400) * 86_400 - 86_400,
            domain: "example.com",
            status: "sent",
            destinations: ["mailto:tls-reports@example.com"],
            error: "",
            successful: 42,
            failed: 0,
            updatedAt: now - 3600,
          },
          {
            day: Math.floor(now / 86_400) * 86_400 - 86_400,
            domain: "example.net",
            status: "failed",
            destinations: ["https://tlsrpt.example.net/v1"],
            error: "https://tlsrpt.example.net/v1: the answer was 503 Service Unavailable",
            successful: 7,
            failed: 2,
            updatedAt: now - 3600,
          },
          {
            day: Math.floor(now / 86_400) * 86_400 - 2 * 86_400,
            domain: "example.org",
            status: "none",
            destinations: [],
            error: "",
            successful: 3,
            failed: 0,
            updatedAt: now - 86_400 - 3600,
          },
        ],
      } satisfies SentTlsReports,
    ],
  ],
  [
    "GET",
    /^\/api\/admin\/reports$/,
    () => [
      200,
      {
        days: 30,
        since: now - 30 * 86_400,
        domains: domains.map((domain) => {
          const view = reportsFor(domain.name);
          return {
            name: domain.name,
            dmarc: view.dmarc,
            tls: view.tls,
            ownFailing: 0,
            // One domain shows what it looks like when a mailbox sits on the report address.
            reading: { dmarc: domain.name !== "verein.example", tls: true },
          };
        }),
      } satisfies ReportsOverview,
    ],
  ],
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/catch-all$/,
    (body, [name]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      const login = (body as { login: string | null }).login;
      if (login && found.kind === "masked") return problem(409, "maskedOnlyDomain");
      found.catchAll = login;
      log("domain.catchAll", name!, { account: found.catchAll });
      return [200, detail(found)];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/forwards$/,
    (body, [name]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      const { local, targets, note } = body as { local: string; targets: string[]; note: string };
      if (found.kind === "masked") return problem(409, "maskedOnlyDomain");
      if (targets.length === 0 || targets.some((target) => !target.includes("@"))) return problem(422, "invalid");
      const address = `${local.toLowerCase()}@${name}`;
      const others = (found.forwards ?? []).filter((forward) => forward.address !== address);
      const createdAt = Math.floor(Date.now() / 1000);
      found.forwards = [...others, { address, domain: name!, targets, note, createdAt }];
      log("domain.forwardAddress", address, { targets });
      return [200, detail(found)];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/domains\/([^/]+)\/forwards\/([^/]+)$/,
    (_, [name, local]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      found.forwards = (found.forwards ?? []).filter((forward) => forward.address !== `${local}@${name}`);
      log("domain.forwardAddressRemove", `${local}@${name}`);
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
      const input = body as {
        address: string;
        name: string;
        admin: boolean;
        service?: boolean;
        makePassword?: boolean;
        quotaBytes: number;
        password?: string;
      };
      const login = input.address.toLowerCase();
      if (maskedOnly(login)) return problem(409, "maskedOnlyDomain");
      if (people.some((p) => p.addresses.some((a) => a.address === login))) return problem(409, "conflict");
      const created = person(login, input.name, {
        role: input.service ? "service" : input.admin ? "admin" : "user",
        protocols: input.service ? serviceProtocols() : allProtocols(),
        quotaBytes: input.quotaBytes,
        usedBytes: 0,
        status: input.service || input.password ? "active" : "invited",
        createdAt: Math.floor(Date.now() / 1000),
      });
      people.push(created);
      people.sort((a, b) => a.login.localeCompare(b.login));
      log("account.create", login, { invited: !input.service && !input.password });
      const access = input.service && input.makePassword ? newAppPassword(login, "Access") : null;
      return [201, { person: created, link: input.service || input.password ? null : link(), access }];
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
      const forwarding = me
        ? { externalBlocked: false, targets: mockForwarding.targets.length, external: 1 }
        : { externalBlocked: found.login === "opa@verein.example", targets: 0, external: 0 };
      const sendAsDomains = mockSendAs[found.login] ?? [];
      const appPasswordList = found.role === "service" ? (servicePasswords[found.login] ?? []) : undefined;
      const oauthGrants = me ? mockSecurity.oauthGrants : (mockPersonGrants[found.login] ?? []);
      const authSource = me ? mockSecurity.authSource : (mockAuthSources[found.login] ?? "local");
      return [
        200,
        {
          ...found,
          security,
          forwarding,
          aliasLimit: me ? mockAddresses.limit : 10,
          sendAsDomains,
          appPasswordList,
          members: found.sharedMailbox ? (sharedMembers[found.login] ?? []) : undefined,
          oauthGrants,
          authSource,
          maskedPolicy: personMaskedPolicy(found.login),
        },
      ];
    },
  ],
  [
    "PATCH",
    /^\/api\/admin\/people\/([^/]+)$/,
    (body, [login]) => {
      const found = people.find((p) => p.login === login);
      if (!found) return problem(404, "notFound");
      const changes = body as {
        name?: string;
        admin?: boolean;
        service?: boolean;
        protocols?: Protocols;
        redirectTo?: string;
        quotaBytes?: number;
        disabled?: boolean;
      };
      if (login === "lorin@uwu.example" && (changes.admin === false || changes.disabled || changes.service)) {
        return problem(409, changes.admin === false ? "lastAdmin" : "notYourself");
      }
      if (changes.name !== undefined) found.name = changes.name;
      if (changes.admin !== undefined) found.role = changes.admin ? "admin" : "user";
      if (changes.service !== undefined) found.role = changes.service ? "service" : "user";
      if (changes.protocols !== undefined) {
        found.protocols = changes.protocols;
        found.hasMailbox = changes.protocols.imap || changes.protocols.jmap;
      }
      if (changes.redirectTo !== undefined) {
        if (changes.redirectTo && !people.some((p) => p.addresses.some((a) => a.address === changes.redirectTo))) {
          return problem(422, "invalid");
        }
        found.redirectTo = changes.redirectTo;
      }
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
    /^\/api\/admin\/people\/([^/]+)\/app-passwords$/,
    (body, [login]) => {
      const found = people.find((p) => p.login === login);
      if (!found) return problem(404, "notFound");
      if (found.role !== "service") return problem(409, "notAService");
      const name = (body as { name?: string }).name?.trim() || "Access";
      const created = newAppPassword(found.login, name);
      log("account.appPasswordCreated", found.login, { name });
      return [201, created];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/people\/([^/]+)\/app-passwords\/(\d+)$/,
    (_, [login, id]) => {
      const list = servicePasswords[login!] ?? [];
      const at = list.findIndex((entry) => entry.id === Number(id));
      if (at < 0) return problem(404, "notFound");
      log("account.appPasswordRevoked", login!, { name: list[at]!.name });
      list.splice(at, 1);
      return [204, null];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/people\/([^/]+)\/oauth-grants\/(\d+)$/,
    (_, [login, id]) => {
      const list = login === "lorin@uwu.example" ? mockSecurity.oauthGrants : (mockPersonGrants[login!] ?? []);
      const at = list.findIndex((grant) => grant.id === Number(id));
      if (at < 0) return problem(404, "notFound");
      log("account.oauthRevoked", login!, { name: list[at]!.clientName });
      list.splice(at, 1);
      return [204, null];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/people\/([^/]+)\/auth-source$/,
    (body, [login]) => {
      const found = people.find((p) => p.login === login);
      if (!found) return problem(404, "notFound");
      if (found.role === "service") return problem(409, "serviceAccount");
      const { source } = body as { source: string };
      if (source !== "local" && source !== "ldap") return problem(422, "invalid");
      if (login === "lorin@uwu.example") {
        mockSecurity.authSource = source;
        if (source === "ldap") mockSecurity.hasPassword = false;
      } else mockAuthSources[login!] = source;
      log("account.authSource", login!, { source });
      return [204, null];
    },
  ],
  ["GET", /^\/api\/admin\/updates$/, () => [200, mockUpdates]],
  [
    "PUT",
    /^\/api\/admin\/updates$/,
    (body) => {
      mockUpdates.settings = body as UpdatesView["settings"];
      return [200, mockUpdates];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/updates\/check$/,
    () => {
      mockUpdates.info.checkedAt = Math.floor(Date.now() / 1000);
      return [200, mockUpdates];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/backups$/,
    () => {
      stepMailbox();
      return [200, stepRestore()];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/backups\/mailbox\/open$/,
    (body) => {
      const job = mockBackups.mailboxRestore;
      if (job.state === "opening" || job.state === "restoring") return problem(409, "backupBusy");
      mailboxStartedAt = Date.now();
      Object.assign(job, {
        state: "opening",
        snapshot: (body as { snapshot?: string }).snapshot ?? "latest",
        createdAt: Math.floor(Date.now() / 1000) - 86_400,
        error: "",
        doneBytes: 0,
        totalBytes: 48_000_000,
        people: [],
        last: null,
      });
      return [200, mockBackups];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/backups\/mailbox\/restore$/,
    (body) => {
      const job = mockBackups.mailboxRestore;
      const { account, folders } = body as { account: string; folders: number[] | null };
      if (job.state !== "open") return problem(422, "invalid");
      const person = job.people.find((candidate) => candidate.login === account);
      if (!person) return problem(422, "invalid");
      mailboxStartedAt = Date.now();
      const total = folders
        ? person.folders.filter((folder) => folders.includes(folder.id)).reduce((sum, folder) => sum + folder.emails, 0)
        : person.emails;
      Object.assign(job, { state: "restoring", account, total, done: 0, restored: 0, skipped: 0 });
      log("backup.restoreMailbox", account);
      return [200, mockBackups];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/backups\/mailbox$/,
    () => {
      Object.assign(mockBackups.mailboxRestore, {
        state: "",
        snapshot: "",
        createdAt: 0,
        error: "",
        people: [],
        account: "",
        total: 0,
        done: 0,
        last: null,
      });
      return [200, mockBackups];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/backups\/restore$/,
    (body) => {
      const snapshot = (body as { snapshot?: string }).snapshot ?? "latest";
      restoreStartedAt = Date.now();
      mockBackups.restore.fetching = {
        state: "fetching",
        snapshot,
        error: "",
        startedAt: Math.floor(Date.now() / 1000),
        doneBytes: 0,
        totalBytes: 2_310_000_000,
      };
      return [200, mockBackups];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/backups\/restore$/,
    () => {
      mockBackups.restore.last = null;
      mockBackups.restore.staged = null;
      mockBackups.restore.fetching = {
        state: "idle",
        snapshot: "",
        error: "",
        startedAt: 0,
        doneBytes: 0,
        totalBytes: 0,
      };
      return [200, mockBackups];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/backups$/,
    (body) => {
      const next = body as {
        enabled: boolean;
        hour: number;
        minute: number;
        encrypted: boolean;
        retention: BackupsView["retention"];
        target: Record<string, unknown> & { kind: BackupTarget["kind"] };
      };
      const newKey = next.encrypted && !mockBackups.encrypted;
      const given = next.target;
      const target: BackupTarget =
        given.kind === "s3"
          ? {
              kind: "s3",
              endpoint: String(given.endpoint ?? ""),
              region: String(given.region || "us-east-1"),
              bucket: String(given.bucket ?? ""),
              prefix: String(given.prefix ?? ""),
              accessKey: String(given.accessKey ?? ""),
              secretKeySet: true,
              pathStyle: Boolean(given.pathStyle),
            }
          : given.kind === "folder"
            ? { kind: "folder", path: String(given.path ?? "") }
            : {
                kind: "sftp",
                host: String(given.host ?? ""),
                port: Number(given.port ?? 22),
                user: String(given.user ?? ""),
                path: String(given.path ?? ""),
                method: given.method === "password" ? "password" : "key",
                publicKey:
                  "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleExampleExampleExampleExampleExample uwumail-backup@mail.uwu.example",
                passwordSet: given.method === "password",
                hostKey: "SHA256:uwuExampleHostKeyFingerprint0000000000000000",
              };
      Object.assign(mockBackups, {
        enabled: next.enabled,
        hour: next.hour,
        minute: next.minute,
        retention: next.retention,
        encrypted: next.encrypted,
        target,
      });
      log("backup.settings", "server");
      return [
        200,
        newKey
          ? { ...mockBackups, recoveryKey: "ABCD-EFGH-IJKL-MNOP-QRST-UVWX-YZ23-4567-ABCD-EFGH-IJKL-MNOP-QRST" }
          : mockBackups,
      ];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/backups\/test$/,
    () => {
      const target = mockBackups.target!;
      return [200, { kind: target.kind, hostKey: target.kind === "sftp" ? target.hostKey : null, known: true }];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/backups\/run$/,
    () => {
      mockBackups.running = true;
      window.setTimeout(() => {
        mockBackups.running = false;
        mockBackups.status.lastSuccessAt = Math.floor(Date.now() / 1000);
      }, 4000);
      return [202, null];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/backups\/snapshots$/,
    () => [
      200,
      [0, 1, 2, 9].map((days): BackupSnapshot => ({
        name: `0017900${days}0000-a1b2c3`,
        createdAt: Math.floor(Date.now() / 1000) - days * 86_400 - 5 * 3600,
        mails: 3300 - days * 4,
        size: 2_310_000_000 - days * 2_000_000,
        uploaded: days === 9 ? 2_100_000_000 : 18_000_000,
        version: "0.1.0",
      })),
    ],
  ],
  [
    "POST",
    /^\/api\/admin\/backups\/recovery-key$/,
    () => [200, { recoveryKey: "ABCD-EFGH-IJKL-MNOP-QRST-UVWX-YZ23-4567-ABCD-EFGH-IJKL-MNOP-QRST" }],
  ],
  [
    "PUT",
    /^\/api\/admin\/people\/([^/]+)\/send-as-domains$/,
    (body, [login]) => {
      const { domains: chosen } = body as { domains: string[] };
      if (chosen.some((name) => maskedOnly(`x@${name}`))) return problem(409, "maskedOnlyDomain");
      mockSendAs[login!] = [...new Set(chosen)].sort();
      log("account.sendAsDomains", login!, { domains: mockSendAs[login!] });
      return [200, { domains: mockSendAs[login!] }];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/people\/([^/]+)\/alias-limit$/,
    (body, [login]) => {
      if (login === "lorin@uwu.example") mockAddresses.limit = (body as { limit: number }).limit;
      log("account.aliasLimit", login ?? "", body as Record<string, unknown>);
      return [204, null];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/domains\/([^/]+)\/groups$/,
    (body, [name]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      const input = body as {
        local: string;
        name: string;
        whoMaySend: WhoMaySend;
        membersMaySendAs: boolean;
        members: string[];
      };
      const address = `${input.local.trim().toLowerCase()}@${name}`;
      if (found.kind === "masked") return problem(409, "maskedOnlyDomain");
      const taken =
        people.some((p) => p.addresses.some((a) => a.address === address)) ||
        (found.groups ?? []).some((group) => group.address === address) ||
        (found.forwards ?? []).some((forward) => forward.address === address);
      if (taken) return problem(409, "conflict");
      const group: GroupInfo = {
        id: Math.floor(Math.random() * 100_000),
        address,
        domain: name!,
        name: input.name,
        whoMaySend: input.whoMaySend,
        membersMaySendAs: input.membersMaySendAs,
        members: groupMembers(input.members),
        createdAt: Math.floor(Date.now() / 1000),
      };
      found.groups = [...(found.groups ?? []), group].sort((a, b) => a.address.localeCompare(b.address));
      log("group.create", address, { members: input.members });
      return [201, group];
    },
  ],
  [
    "PATCH",
    /^\/api\/admin\/domains\/([^/]+)\/groups\/([^/]+)$/,
    (body, [name, local]) => {
      const group = domains.find((d) => d.name === name)?.groups?.find((entry) => entry.address === `${local}@${name}`);
      if (!group) return problem(404, "notFound");
      const input = body as {
        name?: string;
        whoMaySend?: WhoMaySend;
        membersMaySendAs?: boolean;
        members?: string[];
      };
      if (input.name !== undefined) group.name = input.name;
      if (input.whoMaySend) group.whoMaySend = input.whoMaySend;
      if (input.membersMaySendAs !== undefined) group.membersMaySendAs = input.membersMaySendAs;
      if (input.members) group.members = groupMembers(input.members);
      log("group.update", group.address, input as Record<string, unknown>);
      return [200, group];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/domains\/([^/]+)\/groups\/([^/]+)$/,
    (_, [name, local]) => {
      const found = domains.find((d) => d.name === name);
      if (!found) return problem(404, "notFound");
      found.groups = (found.groups ?? []).filter((group) => group.address !== `${local}@${name}`);
      log("group.remove", `${local}@${name}`);
      return [204, null];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/masked-policy$/,
    (body, [name]) => {
      const domain = domains.find((entry) => entry.name === name);
      if (!domain) return problem(404, "notFound");
      if (domain.kind === "masked") return problem(409, "maskedOnlyDomain");
      const input = body as DomainMaskedPolicy;
      const maskedDomains = [...new Set(input.maskedDomains)].sort();
      if (maskedDomains.some((entry) => !maskedDomainNames().includes(entry))) return problem(409, "notMaskedDomain");
      const allowed = [
        ...(input.mode === "own" || input.mode === "both" ? [domain.name] : []),
        ...(input.mode === "dedicated" || input.mode === "both" ? maskedDomains : []),
      ];
      if (input.defaultDomain && !allowed.includes(input.defaultDomain)) return problem(409, "maskedDefault");
      domain.maskedPolicy = { mode: input.mode, maskedDomains, defaultDomain: input.defaultDomain ?? null };
      log("domain.maskedPolicy", domain.name, { ...domain.maskedPolicy });
      return [200, detail(domain)];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/kind$/,
    (body, [name]) => {
      const domain = domains.find((entry) => entry.name === name);
      if (!domain) return problem(404, "notFound");
      const kind = (body as { kind: DomainKind }).kind;
      if ((domain.kind ?? "mail") === kind) return [200, detail(domain)];
      if (kind === "masked") {
        const blockers = kindBlockers(domain);
        if (Object.values(blockers).some((value) => value === true || (typeof value === "number" && value > 0))) {
          return [409, { code: "kindChangeBlocked", detail: "kindChangeBlocked", blockers }];
        }
        domain.kind = "masked";
        domain.maskedPolicy = undefined;
        domain.selfServiceAliases = false;
        log("domain.kind", domain.name, { kind, removedFromDomains: [], removedFromAccounts: [] });
      } else {
        const usedBy = maskedUsedBy(domain.name);
        for (const other of domains) {
          const policy = other.maskedPolicy;
          if (!policy) continue;
          policy.maskedDomains = policy.maskedDomains.filter((entry) => entry !== domain.name);
          if (policy.defaultDomain === domain.name) policy.defaultDomain = null;
        }
        for (const custom of Object.values(mockMaskedCustom)) {
          if (custom.maskedDomains)
            custom.maskedDomains = custom.maskedDomains.filter((entry) => entry !== domain.name);
          if (custom.defaultDomain === domain.name) custom.defaultDomain = null;
        }
        domain.kind = "mail";
        log("domain.kind", domain.name, {
          kind,
          removedFromDomains: usedBy.domains,
          removedFromAccounts: usedBy.accounts,
        });
      }
      return [200, detail(domain)];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/people\/([^/]+)\/masked-policy$/,
    (body, [login]) => {
      const found = people.find((entry) => entry.login === login);
      if (!found) return problem(404, "notFound");
      const input = body as AccountMaskedPolicy;
      if (input.maskedDomains?.some((entry) => !maskedDomainNames().includes(entry))) {
        return problem(409, "notMaskedDomain");
      }
      const custom: AccountMaskedPolicy = {
        mode: input.mode ?? null,
        maskedDomains: input.maskedDomains ? [...new Set(input.maskedDomains)].sort() : null,
        defaultDomain: null,
      };
      const before = mockMaskedCustom[found.login];
      mockMaskedCustom[found.login] = custom;
      if (input.defaultDomain) {
        // Refused as a whole, as the server does.
        if (!effectiveMasked(found.login).domains.includes(input.defaultDomain)) {
          if (before) mockMaskedCustom[found.login] = before;
          else delete mockMaskedCustom[found.login];
          return problem(409, "maskedDefault");
        }
        custom.defaultDomain = input.defaultDomain;
      }
      log("account.maskedPolicy", found.login, { ...custom });
      return [200, personMaskedPolicy(found.login)];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/shared-mailboxes$/,
    () => [
      200,
      people
        .filter((entry) => entry.sharedMailbox)
        .map((entry) => ({ login: entry.login, name: entry.name, members: sharedMembers[entry.login] ?? [] })),
    ],
  ],
  [
    "POST",
    /^\/api\/admin\/shared-mailboxes$/,
    (body) => {
      const input = body as {
        address: string;
        name: string;
        quotaBytes: number;
        members: { login: string; maySend: boolean }[];
      };
      const login = input.address.toLowerCase();
      if (maskedOnly(login)) return problem(409, "maskedOnlyDomain");
      if (people.some((p) => p.addresses.some((a) => a.address === login))) return problem(409, "conflict");
      const created = person(login, input.name, {
        role: "service",
        protocols: { ...serviceProtocols(), smtp: true },
        quotaBytes: input.quotaBytes,
        usedBytes: 0,
        sharedMailbox: true,
        createdAt: Math.floor(Date.now() / 1000),
      });
      people.push(created);
      people.sort((a, b) => a.login.localeCompare(b.login));
      sharedMembers[login] = input.members.map((member, index) => ({
        id: index + 1,
        login: member.login,
        name: people.find((entry) => entry.login === member.login)?.name ?? "",
        maySend: member.maySend,
      }));
      log("sharedMailbox.create", login, { members: input.members });
      return [201, { person: created, members: sharedMembers[login] }];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/shared-mailboxes\/([^/]+)\/members$/,
    (body, [login]) => {
      const found = people.find((entry) => entry.login === login && entry.sharedMailbox);
      if (!found) return problem(404, "notFound");
      const members = (body as { members: { login: string; maySend: boolean }[] }).members;
      sharedMembers[found.login] = members.map((member, index) => ({
        id: index + 1,
        login: member.login,
        name: people.find((entry) => entry.login === member.login)?.name ?? "",
        maySend: member.maySend,
      }));
      log("sharedMailbox.members", found.login, { members });
      return [200, sharedMembers[found.login]];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/people\/([^/]+)\/shared-mailbox$/,
    (body, [login]) => {
      const found = people.find((entry) => entry.login === login);
      if (!found) return problem(404, "notFound");
      if (login === "lorin@uwu.example") return problem(409, "notYourself");
      if (found.sharedMailbox) return problem(409, "sharedMailbox");
      const members = (body as { members: { login: string; maySend: boolean }[] }).members;
      if (members.some((member) => member.login === found.login)) return problem(422, "invalid");
      const from = found.role === "service" ? "service" : "person";
      found.role = "service";
      found.sharedMailbox = true;
      if (!found.hasMailbox) {
        found.protocols = { ...found.protocols, imap: true, jmap: true };
        found.hasMailbox = true;
      }
      sharedMembers[found.login] = members.map((member, index) => ({
        id: index + 1,
        login: member.login,
        name: people.find((entry) => entry.login === member.login)?.name ?? "",
        maySend: member.maySend,
      }));
      log("sharedMailbox.convert", found.login, { from, members });
      return [200, { person: found, members: sharedMembers[found.login] }];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/people\/([^/]+)\/shared-mailbox$/,
    (_body, [login]) => {
      const found = people.find((entry) => entry.login === login && entry.sharedMailbox);
      if (!found) return problem(404, "notFound");
      found.sharedMailbox = false;
      delete sharedMembers[found.login];
      log("sharedMailbox.end", found.login, {});
      return [200, found];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/domains\/([^/]+)\/self-service$/,
    (body, [name]) => {
      const domain = domains.find((entry) => entry.name === name);
      if (domain) domain.selfServiceAliases = (body as { on: boolean }).on;
      log("domain.selfServiceAliases", name ?? "", body as Record<string, unknown>);
      return [204, null];
    },
  ],
  [
    "PUT",
    /^\/api\/admin\/people\/([^/]+)\/external-forwarding$/,
    (body, [login]) => {
      log("account.externalForwarding", login ?? "", body as Record<string, unknown>);
      return [204, null];
    },
  ],
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
      if (maskedOnly(value)) return problem(409, "maskedOnlyDomain");
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
  // An uploaded logo stays in this tab as an object URL.
  if (init?.body instanceof Blob && url.pathname === "/api/admin/branding/logo") {
    mockLogo = URL.createObjectURL(init.body);
  }
  if (init?.body instanceof Blob && /\/(picture|logo)$/.test(url.pathname)) {
    mockPictureUpload = URL.createObjectURL(init.body);
  }
  await new Promise((resolve) => setTimeout(resolve, 250));
  // The BIMI logo preview is an SVG, not JSON.
  const bimiLogo = /^\/api\/admin\/domains\/([^/]+)\/bimi\/logo\.svg$/.exec(url.pathname);
  if (method === "GET" && bimiLogo) {
    const svg = bimiOf(decodeURIComponent(bimiLogo[1]!)).svg;
    if (svg) return new Response(svg, { status: 200, headers: { "Content-Type": "image/svg+xml" } });
  }
  let result: [number, unknown] = problem(404, "notFound");
  for (const [routeMethod, pattern, handler] of routes) {
    const match = routeMethod === method ? pattern.exec(url.pathname) : null;
    if (match) {
      result = handler(body, match.slice(1).map(decodeURIComponent), url.searchParams);
      break;
    }
  }
  const [status, data] = result;
  return new Response(status === 204 ? null : JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
};
