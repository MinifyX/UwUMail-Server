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

export type HealthLevel = "ok" | "unknown" | "warning" | "problem";
export type HealthAreaName = "dns" | "certificate" | "gateway" | "delivery" | "storage" | "security";

export interface HealthFinding {
  code: string;
  level: HealthLevel;
  params?: Record<string, unknown>;
  /** Portal page where it can be fixed. */
  link?: string;
}

export interface HealthArea {
  area: HealthAreaName;
  level: HealthLevel;
  findings: HealthFinding[];
}

export interface Health {
  level: HealthLevel;
  checkedAt: number | null;
  areas: HealthArea[];
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
  /** Only in the detail view. */
  security?: PersonSecurity;
  forwarding?: { externalBlocked: boolean; targets: number; external: number };
  aliasLimit?: number;
  /** Domains the person may send as with any address; only in the detail view. */
  sendAsDomains?: string[];
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

export type RecordKind =
  | "mx"
  | "spf"
  | "dmarc"
  | "dkim"
  | "tlsrpt"
  | "mtasts"
  | "mtastsHost"
  | "mtastsPolicy"
  | "jmap"
  | "imaps"
  | "submissions"
  | "submission";

export interface RecordCheck {
  kind: RecordKind;
  name: string;
  /** HTTPS is the MTA-STS policy file, not a DNS record. */
  recordType: "MX" | "TXT" | "SRV" | "CNAME" | "HTTPS";
  expected: string;
  found: string[];
  status: CheckStatus;
  note: string | null;
  selector: string | null;
  keyState: DkimKeyState | null;
  /** Recommended; it does not count for the domain's status. */
  optional: boolean;
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

export interface ForwardAddress {
  address: string;
  domain: string;
  targets: string[];
  note: string;
  createdAt: number;
}

export interface DomainDetail extends Omit<DomainSummary, "dns"> {
  selfServiceAliases?: boolean;
  forwards: ForwardAddress[];
  keys: DkimKeyInfo[];
  report: DomainReport | null;
  mtaSts: MtaStsView | null;
  setup: { hostname: string; relayHost: string | null; upstreamMx: boolean };
}

export type MtaStsMode = "testing" | "enforce";

export interface MtaStsView {
  mode: MtaStsMode;
  /** The MX names the policy lists. */
  mx: string[];
  changedAt: number;
  policy: string;
  id: string;
}

export interface Reporter {
  organization: string;
  reports: number;
  /** Messages (DMARC) or TLS sessions. */
  count: number;
}

export interface DmarcSource {
  ip: string;
  messages: number;
  passed: number;
  headerFrom: string[];
  /** One of the addresses this server sends from. */
  ours: boolean;
}

export interface ReportPeriod {
  reports: number;
  /** Report mails that did not pass DMARC themselves. */
  unauthenticated: number;
  firstBegin: number | null;
  lastEnd: number | null;
  reporters: Reporter[];
}

export interface ReportsView {
  days: number;
  dmarc: ReportPeriod & { messages: number; passed: number; sources: DmarcSource[] };
  tls: ReportPeriod & {
    successful: number;
    failed: number;
    failures: { resultType: string; policyType: string; mxHost: string; sessions: number }[];
  };
  suggestions: ({ code: "mtaStsEnforce" } | { code: "dmarcStricter"; params: { from: string; to: string } })[];
}

/** One domain in the Reports section: the same numbers as its own page, side by side with the rest. */
export interface DomainReports {
  name: string;
  /** Unlike the single domain's view, the sources here are not marked as ours one by one. */
  dmarc: ReportPeriod & { messages: number; passed: number; sources: Omit<DmarcSource, "ours">[] };
  tls: ReportsView["tls"];
  /** Sources of ours that failed DMARC in the period. */
  ownFailing: number;
  /** False when someone claimed dmarc-reports@ or tls-reports@ as a mailbox or alias. */
  reading: { dmarc: boolean; tls: boolean };
}

export interface ReportsOverview {
  days: number;
  since: number;
  domains: DomainReports[];
}

export type ReportKind = "dmarc" | "tls";

/** One report in a list. `good` and `bad` are messages for DMARC and sessions for TLS. */
export interface ReportEntry {
  id: number;
  organization: string;
  reportId: string;
  beginAt: number;
  endAt: number;
  receivedAt: number;
  /** The report mail itself passed DMARC, so the sender is who they say. */
  authenticated: boolean;
  good: number;
  bad: number;
  about: string | null;
  policy: string | null;
}

/** One line of a DMARC report: what one sending address did, and what was checked. */
export interface DmarcReportRow {
  sourceIp: string;
  messages: number;
  dkimAligned: boolean;
  spfAligned: boolean;
  disposition: string;
  headerFrom: string;
  dkimDomain: string | null;
  dkimSelector: string | null;
  dkimResult: string | null;
  spfDomain: string | null;
  spfResult: string | null;
  overrideReason: string | null;
  envelopeFrom: string | null;
  envelopeTo: string | null;
  ours: boolean;
}

export interface TlsReportFailure {
  policyType: string;
  resultType: string;
  mxHost: string;
  sendingIp: string;
  sessions: number;
  failureCode: string | null;
  receivingIp: string | null;
  helo: string | null;
  detail: string | null;
}

/** What the spam filter did with one message. */
export type SpamLogAction = "delivered" | "junk" | "greylist" | "reject" | "dmarc" | "blocked";

export interface SpamLogEntry {
  id: number;
  at: number;
  smtpId: string;
  messageId: string | null;
  action: SpamLogAction;
  envelopeFrom: string;
  headerFrom: string;
  /** Only kept for mail the filter held back, unless an admin asked for the rest too. */
  subject: string | null;
  clientIp: string;
  helo: string;
  reverseName: string | null;
  size: number;
  score: number | null;
  hits: { rule: string; points: number; detail: string | null }[];
  /** The Authentication-Results line in full. */
  auth: string | null;
  recipients: { address: string; action: string; mailbox: string | null }[];
  /** What someone said later with Spam / Not spam; null means nobody said anything. */
  correctedToJunk: boolean | null;
}

export interface SpamLogView {
  entries: SpamLogEntry[];
  total: number;
  oldest: number | null;
  settings: { enabled: boolean; cleanSubjects: boolean; retentionDays: number };
  maxRows: number;
}

export type ReportDetail =
  | { kind: "dmarc"; report: ReportEntry; rows: DmarcReportRow[] }
  | { kind: "tls"; report: ReportEntry; policy: string | null; failures: TlsReportFailure[] };

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

export interface QueueRecipient {
  address: string;
  status: "pending" | "delivered" | "failed";
  attempts: number;
  nextAttemptAt: number;
  lastError: string | null;
}

export interface QueuedMessage {
  id: number;
  /** Empty for notices (bounces) to a sender. */
  from: string;
  size: number;
  createdAt: number;
  expiresAt: number;
  recipients: QueueRecipient[];
}

export interface LogLine {
  seq: number;
  /** Milliseconds since 1970. */
  at: number;
  level: "error" | "warn" | "info" | "debug" | "trace";
  target: string;
  message: string;
  fields: [string, string][];
}

export type SettingSource = "default" | "database" | "file";

export interface SettingValue {
  key: string;
  value: unknown;
  set: boolean;
  source: SettingSource;
}

export interface BayesTotals {
  spam: number;
  ham: number;
}

export type UpdateChannel = "stable" | "beta";

export interface UpdatesView {
  build: { version: string; commit: string | null; release: boolean };
  settings: { check: boolean; channel: UpdateChannel };
  info: {
    checkedAt: number | null;
    error: string | null;
    releases: { version: string; name: string; notes: string; publishedAt: string; url: string; prerelease: boolean }[];
    behind: number | null;
    commits: { sha: string; message: string }[];
  };
  image: string;
  serverCommand: string;
  gateway: { software: string } | null;
  gatewayCommand: string | null;
}

export interface BackupReport {
  snapshot: string;
  uploaded: number;
  total: number;
  removedSnapshots: number;
  removedObjects: number;
}

export interface BackupsView {
  enabled: boolean;
  /** The hour in UTC the daily backup starts at, and the minute of it. */
  hour: number;
  minute: number;
  retention: { daily: number; weekly: number; monthly: number };
  encrypted: boolean;
  target: {
    host: string;
    port: number;
    user: string;
    path: string;
    method: "key" | "password";
    publicKey: string | null;
    passwordSet: boolean;
    hostKey: string | null;
  } | null;
  status: {
    lastAttemptAt: number | null;
    lastSuccessAt: number | null;
    /** When the run that is going on, or the last one, began and ended. */
    startedAt: number | null;
    finishedAt: number | null;
    lastError: string | null;
    lastReport: BackupReport | null;
  };
  running: boolean;
}

export interface BackupSnapshot {
  name: string;
  createdAt: number;
  mails: number;
  size: number;
  uploaded: number;
  version: string;
}

export interface SpamLimits {
  junk: number | null;
  reject: number | null;
}

export interface SpamLimitsView {
  own: SpamLimits;
  server: { junk: number; reject: number | null };
  min: number;
  max: number;
}

export interface AccountSpamView {
  limits: SpamLimitsView;
  bayes: { enabled: boolean; minimum: number; own: BayesTotals; server: BayesTotals };
}

export interface AdminSpamView {
  bayes: { enabled: boolean; minimum: number; server: BayesTotals; queued: number };
}

export type SenderListName = "allow" | "block";
export type SenderKind = "ip" | "host" | "address" | "domain" | "pattern";

export interface SenderListEntry {
  id: number;
  list: SenderListName;
  kind: SenderKind;
  value: string;
  note: string;
  /** The domain of a domain-wide entry; null for the whole server and for one's own. */
  domain: string | null;
  createdAt: number;
  createdBy: string;
}

export interface SendersView {
  entries: SenderListEntry[];
  limit: number;
  /** Admins only: the domains an entry can be for. */
  domains?: string[];
}

export interface NewSender {
  list: SenderListName;
  value: string;
  kind?: SenderKind;
  note?: string;
  domain?: string;
}

export interface WordEntry {
  id: number;
  pattern: string;
  points: number | null;
  note: string;
  domain: string | null;
  createdAt: number;
  createdBy: string;
}

export interface WordSource {
  id: number;
  url: string;
  subjectOnly: boolean;
  points: number | null;
  domain: string | null;
  fetchedAt: number | null;
  error: string | null;
  entries: number;
  createdAt: number;
  createdBy: string;
}

export interface WordsView {
  entries: WordEntry[];
  sources: WordSource[];
  limit: number;
  sourceLimit: number;
  defaultPoints: number;
  maxPoints: number;
  /** Admins only: the domains an entry can be for. */
  domains?: string[];
}

export interface WordImport {
  added: number;
  duplicates: number;
  refused: { line: string; reason: string }[];
  refusedCount: number;
}

export interface FeedStatus {
  key: string;
  source: string;
  page: string;
  needsKey: boolean;
  intervalSecs: number;
  active: boolean;
  fetchedAt: number | null;
  changedAt: number | null;
  error: string | null;
  entries: number;
}

export interface FeedsView {
  feeds: FeedStatus[];
  abuseChKeySet: boolean;
  /** Only after fetching one now: why it failed. */
  error?: string | null;
}

export interface LearnedFromFolders {
  spam: number;
  ham: number;
  people?: number;
}

export interface SettingsView {
  settings: SettingValue[];
  configFile: string | null;
  /** Runtime, not a setting: with a gateway paired, mail leaves through it whatever the route says. */
  gateway: { paired: boolean };
}

/** The first login step when the account has a second factor: no session yet. */
export interface SecondFactorChallenge {
  token: string;
  totp: boolean;
  passkey: boolean;
  recoveryCodes: boolean;
}

export type LoginResult = Session | { secondFactor: SecondFactorChallenge };

export const needsSecondFactor = (result: LoginResult): result is { secondFactor: SecondFactorChallenge } =>
  "secondFactor" in result;

export type AppScope = "mail" | "smtp" | "dav";

export interface AppPasswordInfo {
  id: number;
  name: string;
  scopes: AppScope[];
  createdAt: number;
  expiresAt: number | null;
  lastUsedAt: number | null;
  lastUsedProtocol: string | null;
  lastUsedIp: string | null;
}

export interface PasskeyInfo {
  id: number;
  name: string;
  createdAt: number;
  lastUsedAt: number | null;
}

export interface WebSessionInfo {
  id: string;
  createdAt: number;
  lastSeenAt: number;
  ip: string;
  userAgent: string;
  current: boolean;
}

export interface SecurityEventInfo {
  id: number;
  at: number;
  kind: string;
  actor: string;
  ip: string;
  details: Record<string, unknown>;
}

export interface SecurityView {
  totp: boolean;
  passkeys: PasskeyInfo[];
  recoveryCodesLeft: number;
  secondFactor: boolean;
  appsNeedAppPassword: boolean;
  appPasswordsRequired: boolean;
  appPasswords: AppPasswordInfo[];
  sessions: WebSessionInfo[];
  events: SecurityEventInfo[];
}

export interface TotpSetup {
  secret: string;
  uri: string;
  qr: { size: number; modules: string } | null;
}

export interface PersonSecurity {
  secondFactor: boolean;
  totp: boolean;
  passkeys: number;
  appPasswords: number;
  appPasswordsRequired: boolean;
}

export interface ForwardTargetInfo {
  id: number;
  address: string;
  local: boolean;
  createdAt: number;
  confirmedAt: number | null;
}

export interface ForwardingView {
  keepCopy: boolean;
  externalAllowed: boolean;
  targets: ForwardTargetInfo[];
  maxTargets: number;
}

export interface VacationView {
  isEnabled: boolean;
  fromDate: number | null;
  toDate: number | null;
  subject: string | null;
  textBody: string | null;
}

export interface ForwardLinkInfo {
  address: string;
  from: string;
  name?: string;
}

export interface OwnAddressesView {
  addresses: { address: string; kind: "primary" | "alias"; own: boolean; createdAt: number }[];
  domains: string[];
  limit: number;
  used: number;
  released: { address: string; releasedAt: number; reservedUntil: number }[];
}

export interface StorageView {
  usedBytes: number;
  quotaBytes: number;
  mailboxes: {
    id: number;
    name: string;
    role: "inbox" | "drafts" | "sent" | "archive" | "junk" | "trash" | null;
    emails: number;
    sizeBytes: number;
  }[];
}

/** The setup assistant before the first admin exists. */
export interface SetupStatus {
  open: boolean;
  hostname: string;
  /** Domains added on the command line, only while setup is open. */
  domains: string[];
}

export type ProbeStage = "dns" | "connect" | "tls" | "login";

/** How mail leaves: straight to other servers, through a relay, or straight but from the UwUMail Gateway. */
export type DeliveryRoute = "direct" | "relay" | "gateway";

export interface ProbeReport {
  at: number;
  route: DeliveryRoute;
  target: string;
  ok: boolean;
  stage: ProbeStage | null;
  error: string | null;
}

export interface Listing {
  list: string;
  status: "clean" | "listed" | "unknown";
  answer: string | null;
}

export interface AddressReport {
  ip: string;
  private: boolean;
  ptr: string[];
  ptrConfirmed: boolean;
  ptrIsHostname: boolean;
  listings: Listing[];
}

export interface InboundReport {
  ip: string;
  reachable: boolean;
  ours: boolean;
  greeting: string | null;
  error: string | null;
}

export interface ServerCheck {
  checkedAt: number;
  hostname: string;
  addresses: AddressReport[];
  route: DeliveryRoute;
  relayHost: string | null;
  relayAddresses: AddressReport[];
  outbound: ProbeReport;
  inbound: InboundReport[];
  upstream: boolean;
  blocklistsChecked: boolean;
}

/** A network whose operator blocks or restricts outgoing port 25. */
export interface ReachabilityProvider {
  key: "hetzner" | "strato" | "ionos";
  advice: "avoid" | "askSupport";
  source: string;
}

export interface PublicAddress {
  ip: string;
  ptr: string[];
  genericPtr: boolean;
  /** Spamhaus PBL: a home or dynamic connection. */
  homeConnection: boolean;
  listed: boolean;
  spamhausUnknown: boolean;
  asn: number | null;
  network: string | null;
  provider: ReachabilityProvider | null;
}

export interface Reachability {
  checkedAt: number;
  addresses: PublicAddress[];
  outbound: ProbeReport;
  inbound: InboundReport | null;
  throughGateway: boolean;
  recommendation: "direct" | "gateway" | "unknown";
  reasons: string[];
}

export type GatewayState = "none" | "connecting" | "connected" | "refused";

export interface GatewayView {
  state: GatewayState;
  tunnel: string[];
  fingerprint: string | null;
  /** Where the host name has to point. */
  addresses: string[];
  services: string[];
  outboundPorts: number[];
  software: string | null;
  connectedSince: number | null;
  downSince: number | null;
  error: string | null;
  refusal: "notPaired" | "wrongToken" | "otherServer" | "version" | null;
  fromConfig: boolean;
  /** The machine the gateway runs on. Missing while the tunnel is down. */
  machine: GatewayMachine | null;
}

export interface GatewayMachine {
  system: GatewaySystem | null;
  protection: GatewayProtection | null;
  /** Addresses the gateway keeps out of every ban list, this server's among them. */
  trusted: string[];
  checkedAt: number;
}

export interface GatewaySystem {
  /** As the machine calls itself, for example `Ubuntu 26.04.1 LTS`. */
  name: string;
  updates: number;
  securityUpdates: number;
  rebootRequired: boolean;
  /** Whether security updates install themselves. */
  automaticSecurity: boolean;
  newRelease: string | null;
  /** What to run on the gateway to install them. */
  command: string;
}

export interface GatewayProtection {
  /** The firewall in use, for example `ufw`; empty when none was found. */
  firewall: string;
  firewallActive: boolean;
  fail2ban: boolean;
  banned: number;
  jails: string[];
  /** Of those bans, the ones this server asked for. */
  fromServer: number;
}

export interface TestMailSent {
  messageId: string;
  external: string | null;
}

export interface TestMailStatus {
  arrived: boolean;
  replyFrom: string | null;
}

export interface CloudflareResult {
  name: string;
  recordType: "MX" | "TXT";
  outcome: "created" | "updated" | "skipped" | "failed";
  error: string | null;
}
