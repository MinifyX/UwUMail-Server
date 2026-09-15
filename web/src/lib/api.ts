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

export interface DomainDetail extends Omit<DomainSummary, "dns"> {
  selfServiceAliases?: boolean;
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

export interface SettingsView {
  settings: SettingValue[];
  configFile: string | null;
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

export type AppScope = "mail" | "smtp";

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
