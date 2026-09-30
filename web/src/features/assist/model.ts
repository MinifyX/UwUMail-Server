/**
 * The AI assistant's shapes (docs/jmap-assist.md, portal REST API) and the logic of its pages that
 * does not need React: form checks, daily limits typed by hand, the ChatGPT device login and the
 * usage sums.
 */

export const FEATURES = ["compose", "summarize", "spamCheck", "extractEvents", "autoLabels"] as const;
export type Feature = (typeof FEATURES)[number];
/** What a provider's `features` name when it may serve mail of the person's other accounts. */
export const FOREIGN_MAIL = "foreignMail" as const;
/** What a provider may be used for: the features, and mail of other accounts. */
export type ProviderFeature = Feature | typeof FOREIGN_MAIL;

export type ProviderKind =
  "openai" | "anthropic" | "gemini" | "mistral" | "openrouter" | "ollama" | "openaiCompatible" | "chatgpt";

/** The preset of a kind, as the server hands it out. */
export interface KindInfo {
  kind: ProviderKind;
  name: string;
  defaultBaseUrl: string | null;
  /** Whether the form shows a URL field, and whether it has to be filled in. */
  baseUrl: "fixed" | "optional" | "required";
  /** "login" is the ChatGPT device login instead of a key. */
  key: "required" | "optional" | "none" | "login";
  model: string | null;
  fastModel: string | null;
  /** Where to get a key; a link in the form. */
  keyUrl: string | null;
  experimental: boolean;
  /** Only people may add it for themselves, never the admin for the server. */
  personalOnly: boolean;
}

export interface Choice {
  providerId: number;
  model: string | null;
}

export interface Effective {
  providerId: number;
  providerName: string;
  model: string;
  scope: "server" | "personal";
}

export interface ModelsAnswer {
  models: { id: string; name: string }[];
  model: string | null;
  fastModel: string | null;
}

export type Access = "everyone" | "domains" | "people";

/** A higher price once a request's prompt is larger than `aboveTokens`, per million tokens. */
export interface PriceTier {
  aboveTokens: number;
  inputPerMillion: number;
  outputPerMillion: number;
  reasoningPerMillion: number;
  cacheReadPerMillion: number;
}

/** What a model costs, in US dollars per million tokens (and per request, picture, search). */
export interface Price {
  inputPerMillion: number;
  outputPerMillion: number;
  /** The rest of the price sheet, from servers since 0.20.0. */
  reasoningPerMillion?: number;
  cacheReadPerMillion?: number;
  cacheWritePerMillion?: number;
  perRequest?: number;
  perImage?: number;
  webSearchPerQuery?: number;
  tiers?: PriceTier[];
  /** The model thinks before it answers, by the price lists. */
  supportsReasoning?: boolean;
  maxOutputTokens?: number | null;
  /** From the price lists, set by hand, or nothing per request (Ollama, a subscription). */
  source: "auto" | "manual" | "free";
}

/** A cost in the currency asked for, and in US dollars. */
export interface Cost {
  amount: number;
  currency: string;
  usd: number;
}

/** How fresh the server's price lists are. */
export interface PriceLists {
  fetchedAt: number | null;
  models: number;
  openrouterModels: number;
  ratesDay: string | null;
}

export interface AssistPolicy {
  features: Record<Feature, boolean>;
  allowPersonal: boolean;
  allowPersonalPrivate: boolean;
  /** "AI for mail from other accounts" (docs/jmap-assist.md, "Foreign mail"); off by default. */
  foreignMail?: boolean;
}

export interface AdminProvider {
  id: number;
  name: string;
  kind: ProviderKind;
  baseUrl: string | null;
  hasKey: boolean;
  keyHint: string | null;
  model: string | null;
  fastModel: string | null;
  enabled: boolean;
  access: Access;
  domains: string[];
  people: string[];
  features: ProviderFeature[];
  requestsPerDay: number | null;
  tokensPerDay: number | null;
  /** US dollars per million tokens set by hand; null: from the price lists. */
  inputPricePerMillion: number | null;
  outputPricePerMillion: number | null;
  /** US dollars per request set by hand; null (or missing on older servers): from the price lists. */
  pricePerRequest?: number | null;
  /** Whether the people using it see what it costs. */
  showCostToUsers: boolean;
  /** What the default model costs, when known. */
  price: Price | null;
  createdAt: number;
}

export interface AdminAssistView {
  policy: AssistPolicy;
  providers: AdminProvider[];
  kinds: KindInfo[];
  priceLists?: PriceLists;
}

export interface Quota {
  requestsPerDay: number | null;
  tokensPerDay: number | null;
}

export interface AssistProvider {
  id: number;
  name: string;
  kind: ProviderKind;
  scope: "server" | "personal";
  /** Only for one's own providers; the admin's addresses are not shown. */
  baseUrl: string | null;
  hasKey: boolean;
  keyHint: string | null;
  model: string | null;
  fastModel: string | null;
  features: ProviderFeature[];
  quota: Quota | null;
  experimental: boolean;
  /** ChatGPT: signed in. Others: a key is stored or none is needed. */
  connected: boolean;
  /** Null for a server provider whose costs the admin keeps to themselves. */
  inputPricePerMillion?: number | null;
  outputPricePerMillion?: number | null;
  pricePerRequest?: number | null;
  price?: Price | null;
}

export interface AssistSettings {
  default: Choice | null;
  features: Record<Feature, Choice | null>;
  autoLabels: boolean;
  /** Labels put on by their rules, detector, learned senders and classifier, without a model. */
  nonAiLabels?: boolean;
  refineEvents: boolean;
  /** The synced user setting `assist.currency`, for someone reading in English. */
  currency?: "EUR" | "USD" | null;
  effective: Record<Feature, Effective | null>;
}

export interface TodayUsage {
  providerId: number;
  providerName: string;
  requests: number;
  tokens: number;
  requestsPerDay: number | null;
  tokensPerDay: number | null;
  /** Only in the usage view: today's cost, when known and shown. */
  cost?: Cost | null;
}

/** One of the person's labels, as the account page lists it. */
export interface LabelSummary {
  id: number;
  name: string;
  color: string | null;
  detector: string | null;
  hasRules: boolean;
  learnSenders: boolean;
  classifier: boolean;
  totalEmails: number;
  unreadEmails: number;
  /** Mails the classifier learned with the label; it acts from 15 on. */
  examples: number;
}

/** Examples a label's classifier needs before it puts the label on by itself (docs/labels.md). */
export const CLASSIFIER_MIN_EXAMPLES = 15;

export interface AccountAssistView {
  features: Record<Feature, boolean>;
  /** The assistant may be used for mail of the person's other accounts in the UwUMail app. */
  foreignMail?: boolean;
  mayAddProviders: boolean;
  mayUsePrivateAddresses: boolean;
  maxProviders: number;
  providers: AssistProvider[];
  settings: AssistSettings;
  today: TodayUsage[];
  kinds: KindInfo[];
  labels: number;
  labelList?: LabelSummary[];
}

export interface UsageRow {
  day: string;
  /** Only in the admin's view. */
  login?: string;
  providerId: number;
  providerName: string;
  feature: Feature;
  requests: number;
  inputTokens: number;
  /** The answer's text, without thinking. */
  outputTokens: number;
  /** What the model spent thinking; missing from servers before 0.20.0. */
  reasoningTokens?: number;
  /** Of the input, read from the provider's cache. */
  cachedTokens?: number;
  /** Calls to the model. */
  calls?: number;
  /** Null when the price was not known or is not shown. */
  cost?: Cost | null;
}

export interface AccountUsageView {
  days: UsageRow[];
  today: TodayUsage[];
}

export interface AdminUsageView {
  days: UsageRow[];
}

export interface ChatgptLoginStart {
  userCode: string;
  verificationUri: string;
  interval: number;
  expiresAt: number;
}

export interface ChatgptPollAnswer {
  status: "pending" | "connected" | "expired" | "failed";
  description?: string;
}

export const ACCOUNT_ASSIST = "/api/account/assist";
export const ADMIN_ASSIST = "/api/admin/assist";

// ---------------------------------------------------------------------------------------------
// Daily limits

/**
 * A daily limit as typed: empty is no limit, otherwise a whole number above zero. Spaces, dots,
 * commas and apostrophes between digits are read as thousands separators, and "k" or "M" at the
 * end multiply, so "100.000", "100 000" and "100k" are the same.
 */
export function parseLimit(text: string): number | null | "invalid" {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  let value: number;
  const scaled = /^(\d+(?:[.,]\d+)?)\s*([kKmM])$/.exec(trimmed);
  if (scaled) {
    // With a suffix a dot or comma is a decimal point: "1.5k" and "1,5k" are 1500.
    const factor = scaled[2]!.toLowerCase() === "k" ? 1_000 : 1_000_000;
    value = Math.round(Number(scaled[1]!.replace(",", ".")) * factor * 1000) / 1000;
  } else {
    const plain = trimmed.replace(/(\d)[\s.,'’_](?=\d)/g, "$1");
    if (!/^\d+$/.test(plain)) return "invalid";
    value = Number(plain);
  }
  if (!Number.isFinite(value) || !Number.isInteger(value) || value < 1 || value > 1e12) return "invalid";
  return value;
}

/** A stored limit as it goes into the text field again. */
export const limitText = (value: number | null): string => (value === null ? "" : String(value));

/** How much of a limit is used, between 0 and 1; null without a limit. */
export function share(used: number, limit: number | null): number | null {
  if (limit === null || limit <= 0) return null;
  return Math.min(1, Math.max(0, used / limit));
}

// ---------------------------------------------------------------------------------------------
// Costs

export type Currency = "EUR" | "USD" | "JPY" | "CNY";

/**
 * The currency costs are shown in, by the language: yen in Japanese, yuan in Chinese, euros for
 * everyone else, and in English US dollars instead when the person chose them.
 */
export function currencyFor(language: string, chosen?: string | null): Currency {
  const base = language.toLowerCase().split("-")[0];
  if (base === "ja") return "JPY";
  if (base === "zh") return "CNY";
  if (base === "en" && chosen === "USD") return "USD";
  return "EUR";
}

/**
 * An amount of money as the language writes it. Small amounts keep two significant digits
 * ("0.0023 €") and very small ones say so ("< 0.0001 €"), so a cheap request never reads as free.
 */
export function formatCost(amount: number, currency: string, language: string): string {
  const format = (value: number, options: Intl.NumberFormatOptions) =>
    new Intl.NumberFormat(language, { style: "currency", currency, ...options }).format(value);
  const unit = currency === "JPY" ? 1 : 0.01;
  if (amount === 0) return format(0, { maximumFractionDigits: 0 });
  if (amount >= unit) return format(amount, {});
  const floor = unit / 100;
  if (amount < floor) return `< ${format(floor, { maximumSignificantDigits: 1 })}`;
  return format(amount, { maximumSignificantDigits: 2 });
}

/**
 * A price as typed: empty is automatic, otherwise 0 to `max` (comma or dot); per million tokens
 * up to 100,000, per request up to 100 US dollars.
 */
export function parsePrice(text: string, max = 100_000): number | null | "invalid" {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  if (!/^\d+(?:[.,]\d+)?$/.test(trimmed)) return "invalid";
  const value = Number(trimmed.replace(",", "."));
  if (!Number.isFinite(value) || value < 0 || value > max) return "invalid";
  return value;
}

/** The most a request may cost by hand, in US dollars. */
export const MAX_PRICE_PER_REQUEST = 100;

export const priceText = (value: number | null | undefined): string => (value == null ? "" : String(value));

// ---------------------------------------------------------------------------------------------
// The provider form

export interface ProviderDraft {
  name: string;
  kind: ProviderKind;
  baseUrl: string;
  /** A new key; empty keeps the stored one. */
  apiKey: string;
  /** Take the stored key away. */
  removeKey: boolean;
  model: string;
  fastModel: string;
  // Only for the admin's providers.
  enabled: boolean;
  access: Access;
  domains: string[];
  people: string[];
  features: ProviderFeature[];
  requestsPerDay: string;
  tokensPerDay: string;
  /** US dollars per million tokens; empty: from the price lists. */
  inputPrice: string;
  outputPrice: string;
  /** US dollars per request; empty: from the price lists. */
  requestPrice: string;
  /** Only for the admin's providers. */
  showCostToUsers: boolean;
}

export type DraftField =
  | "name"
  | "baseUrl"
  | "apiKey"
  | "access"
  | "features"
  | "requestsPerDay"
  | "tokensPerDay"
  | "inputPrice"
  | "outputPrice"
  | "requestPrice";

/** What is wrong with a field, as the last part of its text key under `assist.form.errors`. */
export type DraftErrors = Partial<Record<DraftField, string>>;

export function emptyDraft(kind: KindInfo | undefined): ProviderDraft {
  return {
    name: kind?.name ?? "",
    kind: kind?.kind ?? "openai",
    baseUrl: kind?.baseUrl === "required" ? (kind.defaultBaseUrl ?? "") : "",
    apiKey: "",
    removeKey: false,
    model: "",
    fastModel: "",
    enabled: true,
    access: "everyone",
    domains: [],
    people: [],
    features: [...FEATURES, FOREIGN_MAIL],
    requestsPerDay: "",
    tokensPerDay: "",
    inputPrice: "",
    outputPrice: "",
    requestPrice: "",
    showCostToUsers: false,
  };
}

/** The form for a provider that is already there. */
export function draftOf(provider: AssistProvider | AdminProvider): ProviderDraft {
  const admin = "access" in provider ? provider : null;
  return {
    name: provider.name,
    kind: provider.kind,
    baseUrl: provider.baseUrl ?? "",
    apiKey: "",
    removeKey: false,
    model: provider.model ?? "",
    fastModel: provider.fastModel ?? "",
    enabled: admin?.enabled ?? true,
    access: admin?.access ?? "everyone",
    domains: admin?.domains ?? [],
    people: admin?.people ?? [],
    features: admin?.features ?? provider.features,
    requestsPerDay: limitText(admin?.requestsPerDay ?? null),
    tokensPerDay: limitText(admin?.tokensPerDay ?? null),
    inputPrice: priceText(provider.inputPricePerMillion),
    outputPrice: priceText(provider.outputPricePerMillion),
    requestPrice: priceText(provider.pricePerRequest),
    showCostToUsers: admin?.showCostToUsers ?? false,
  };
}

/**
 * Picking another kind while adding one: the name and the address follow the kind as long as
 * nobody typed their own, and the models start over.
 */
export function changeKind(draft: ProviderDraft, from: KindInfo | undefined, to: KindInfo): ProviderDraft {
  const nameUntouched = draft.name.trim() === "" || draft.name === from?.name;
  const urlUntouched = draft.baseUrl.trim() === "" || draft.baseUrl === (from?.defaultBaseUrl ?? "");
  return {
    ...draft,
    kind: to.kind,
    name: nameUntouched ? to.name : draft.name,
    baseUrl:
      to.baseUrl === "fixed"
        ? ""
        : urlUntouched
          ? to.baseUrl === "required"
            ? (to.defaultBaseUrl ?? "")
            : ""
          : draft.baseUrl,
    apiKey: to.key === "required" || to.key === "optional" ? draft.apiKey : "",
    model: "",
    fastModel: "",
  };
}

/** What is wrong with an address before the server is asked: the rest (local network) is its call. */
export function urlProblem(text: string): "urlInvalid" | "urlLogin" | null {
  let url: URL;
  try {
    url = new URL(text.trim());
  } catch {
    return "urlInvalid";
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return "urlInvalid";
  if (!url.hostname) return "urlInvalid";
  if (url.username || url.password) return "urlLogin";
  return null;
}

export function validateDraft(
  draft: ProviderDraft,
  kind: KindInfo | undefined,
  context: { hasKey: boolean; admin: boolean },
): DraftErrors {
  const errors: DraftErrors = {};
  const name = draft.name.trim();
  if (!name) errors.name = "nameRequired";
  else if ([...name].length > 60) errors.name = "nameTooLong";

  if (kind && kind.baseUrl !== "fixed") {
    const url = draft.baseUrl.trim();
    if (!url) {
      if (kind.baseUrl === "required") errors.baseUrl = "urlRequired";
    } else {
      const problem = urlProblem(url);
      if (problem) errors.baseUrl = problem;
    }
  }

  if (kind?.key === "required") {
    const willHaveKey = draft.apiKey.trim() !== "" || (context.hasKey && !draft.removeKey);
    if (!willHaveKey) errors.apiKey = "keyRequired";
  }

  if (parsePrice(draft.inputPrice) === "invalid") errors.inputPrice = "priceInvalid";
  if (parsePrice(draft.outputPrice) === "invalid") errors.outputPrice = "priceInvalid";
  if (parsePrice(draft.requestPrice, MAX_PRICE_PER_REQUEST) === "invalid") errors.requestPrice = "requestPriceInvalid";

  if (context.admin) {
    if (draft.access === "domains" && draft.domains.length === 0) errors.access = "domainsRequired";
    if (draft.access === "people" && draft.people.length === 0) errors.access = "peopleRequired";
    if (!draft.features.some((feature) => feature !== FOREIGN_MAIL)) errors.features = "featuresRequired";
    if (parseLimit(draft.requestsPerDay) === "invalid") errors.requestsPerDay = "limitInvalid";
    if (parseLimit(draft.tokensPerDay) === "invalid") errors.tokensPerDay = "limitInvalid";
  }
  return errors;
}

/**
 * The body of a create or update. A key left empty is left out, so the stored one stays; taking
 * it away sends "". Fields the kind has no use for are not sent.
 */
export function providerBody(
  draft: ProviderDraft,
  kind: KindInfo | undefined,
  context: { create: boolean; admin: boolean },
): Record<string, unknown> {
  const body: Record<string, unknown> = { name: draft.name.trim() };
  if (context.create) body.kind = draft.kind;
  if (kind?.baseUrl !== "fixed") {
    const url = draft.baseUrl.trim();
    if (url || !context.create) body.baseUrl = url || null;
  }
  if (kind?.key === "required" || kind?.key === "optional") {
    const key = draft.apiKey.trim();
    if (key) body.apiKey = key;
    else if (draft.removeKey && !context.create) body.apiKey = "";
  }
  body.model = draft.model.trim() || null;
  body.fastModel = draft.fastModel.trim() || null;
  const inputPrice = parsePrice(draft.inputPrice);
  const outputPrice = parsePrice(draft.outputPrice);
  body.inputPricePerMillion = inputPrice === "invalid" ? null : inputPrice;
  body.outputPricePerMillion = outputPrice === "invalid" ? null : outputPrice;
  const requestPrice = parsePrice(draft.requestPrice, MAX_PRICE_PER_REQUEST);
  body.pricePerRequest = requestPrice === "invalid" ? null : requestPrice;
  if (context.admin) {
    body.showCostToUsers = draft.showCostToUsers;
    const requests = parseLimit(draft.requestsPerDay);
    const tokens = parseLimit(draft.tokensPerDay);
    Object.assign(body, {
      enabled: draft.enabled,
      access: draft.access,
      domains: draft.access === "domains" ? draft.domains : [],
      people: draft.access === "people" ? draft.people : [],
      // In the order the features are always listed, whatever order they were ticked in.
      features: [...FEATURES, FOREIGN_MAIL].filter((feature) => draft.features.includes(feature)),
      requestsPerDay: requests === "invalid" ? null : requests,
      tokensPerDay: tokens === "invalid" ? null : tokens,
    });
  }
  return body;
}

/**
 * A chip typed into a list of domains or logins: trimmed, lower case, no leading "@"; null when
 * it is empty or already there.
 */
export function normalizeChip(text: string, existing: string[]): string | null {
  const value = text.trim().replace(/^@/, "").toLowerCase();
  if (!value || existing.includes(value)) return null;
  return value;
}

// ---------------------------------------------------------------------------------------------
// The ChatGPT device login

export type LoginState =
  | { phase: "idle" }
  | { phase: "starting" }
  | { phase: "waiting"; userCode: string; verificationUri: string; interval: number; expiresAt: number }
  | { phase: "connected" }
  | { phase: "expired" }
  | { phase: "failed"; description: string | null };

export type LoginEvent =
  | { type: "start" }
  | { type: "started"; answer: ChatgptLoginStart }
  | { type: "polled"; answer: ChatgptPollAnswer }
  /** The time now, in unix seconds: a code past its end stops the waiting without asking. */
  | { type: "tick"; now: number }
  | { type: "error"; description: string | null }
  | { type: "cancel" };

/** The seconds between polls: what the server asks for, at least one and at most thirty. */
export function pollSeconds(interval: number): number {
  return Number.isFinite(interval) ? Math.min(30, Math.max(1, Math.round(interval))) : 5;
}

export function loginReducer(state: LoginState, event: LoginEvent): LoginState {
  switch (event.type) {
    case "start":
      return state.phase === "starting" || state.phase === "waiting" ? state : { phase: "starting" };
    case "started":
      if (state.phase !== "starting") return state;
      return {
        phase: "waiting",
        userCode: event.answer.userCode,
        verificationUri: event.answer.verificationUri,
        interval: pollSeconds(event.answer.interval),
        expiresAt: event.answer.expiresAt,
      };
    case "polled":
      // An answer that arrives after the login was cancelled or had ended changes nothing.
      if (state.phase !== "waiting") return state;
      switch (event.answer.status) {
        case "pending":
          return state;
        case "connected":
          return { phase: "connected" };
        case "expired":
          return { phase: "expired" };
        case "failed":
          return { phase: "failed", description: event.answer.description ?? null };
      }
      return state;
    case "tick":
      return state.phase === "waiting" && event.now >= state.expiresAt ? { phase: "expired" } : state;
    case "error":
      return state.phase === "starting" || state.phase === "waiting"
        ? { phase: "failed", description: event.description }
        : state;
    case "cancel":
      return { phase: "idle" };
  }
}

// ---------------------------------------------------------------------------------------------
// Usage

export interface UsageSum {
  requests: number;
  inputTokens: number;
  outputTokens: number;
  reasoningTokens: number;
  /** What the rows with a known cost cost together; null when none has one. */
  amount: number | null;
}

const EMPTY: UsageSum = { requests: 0, inputTokens: 0, outputTokens: 0, reasoningTokens: 0, amount: null };

/** One row as a sum. */
export function rowSum(row: UsageRow): UsageSum {
  return {
    requests: row.requests,
    inputTokens: row.inputTokens,
    outputTokens: row.outputTokens,
    reasoningTokens: row.reasoningTokens ?? 0,
    amount: row.cost?.amount ?? null,
  };
}

function add(sum: UsageSum, row: UsageRow): UsageSum {
  const amount = row.cost?.amount;
  return {
    requests: sum.requests + row.requests,
    inputTokens: sum.inputTokens + row.inputTokens,
    outputTokens: sum.outputTokens + row.outputTokens,
    reasoningTokens: sum.reasoningTokens + (row.reasoningTokens ?? 0),
    amount: amount == null ? sum.amount : (sum.amount ?? 0) + amount,
  };
}

export function usageSum(rows: UsageRow[]): UsageSum {
  return rows.reduce(add, EMPTY);
}

/** Whether any row has thinking tokens: then the tables show a column for them. */
export const hasThinking = (rows: { reasoningTokens?: number }[]): boolean =>
  rows.some((row) => (row.reasoningTokens ?? 0) > 0);

/** Whether any row has a cost: then the tables show a cost column. */
export const hasCosts = (rows: UsageRow[]): boolean => rows.some((row) => row.cost != null);

/** Rows added up per day, newest day first. */
export function byDay(rows: UsageRow[]): ({ day: string } & UsageSum)[] {
  const groups = new Map<string, { day: string } & UsageSum>();
  for (const row of rows) {
    groups.set(row.day, { day: row.day, ...add(groups.get(row.day) ?? EMPTY, row) });
  }
  return [...groups.values()].sort((a, b) => b.day.localeCompare(a.day));
}

/** Rows added up per feature, in the order the features are always listed; unused ones are left out. */
export function byFeature(rows: UsageRow[]): ({ feature: Feature } & UsageSum)[] {
  return FEATURES.flatMap((feature) => {
    const own = rows.filter((row) => row.feature === feature);
    return own.length > 0 ? [{ feature, ...usageSum(own) }] : [];
  });
}

/** Rows added up per person over the whole range, the busiest first. */
export function byPerson(rows: UsageRow[]): ({ login: string } & UsageSum)[] {
  const groups = new Map<string, { login: string } & UsageSum>();
  for (const row of rows) {
    const login = row.login ?? "";
    groups.set(login, { login, ...add(groups.get(login) ?? EMPTY, row) });
  }
  return [...groups.values()].sort(
    (a, b) =>
      b.requests - a.requests ||
      b.inputTokens + b.outputTokens - (a.inputTokens + a.outputTokens) ||
      a.login.localeCompare(b.login),
  );
}

/** "2026-09-29" as the language writes a date; the day is a UTC day. */
export function formatDay(day: string, language: string): string {
  const date = new Date(`${day}T00:00:00Z`);
  if (Number.isNaN(date.getTime())) return day;
  return date.toLocaleDateString(language, { timeZone: "UTC", weekday: "short", day: "numeric", month: "short" });
}
