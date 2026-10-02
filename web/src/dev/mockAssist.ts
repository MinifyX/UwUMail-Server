/**
 * The AI assistant for the pretend server: two server providers, one of the person's own, a month
 * of usage, and a ChatGPT login that is confirmed after a few polls. A key that contains "wrong"
 * fails the model test, as does a provider with "unreachable" in its name; a login is confirmed on
 * the third poll unless the provider's name contains "denied".
 */

import {
  FEATURES,
  FOREIGN_MAIL,
  type AccountAssistView,
  type AdminProvider,
  type AssistPolicy,
  type AssistProvider,
  type AssistSettings,
  type Choice,
  type Cost,
  type Effective,
  type Feature,
  type KindInfo,
  type ModelHint,
  type LabelSummary,
  type ProviderFeature,
  type Price,
  type ProviderKind,
  type TodayUsage,
  type UsageRow,
} from "@/features/assist/model";

type Handler = (body: unknown, params: string[], search: URLSearchParams) => [number, unknown];

const problem = (status: number, code: string, detail = code): [number, unknown] => [status, { code, detail }];
const now = () => Math.floor(Date.now() / 1000);

/** The one logged in on the pretend server. */
const ME = "lorin@uwu.example";

const KINDS: KindInfo[] = [
  {
    kind: "openai",
    name: "OpenAI",
    defaultBaseUrl: "https://api.openai.com/v1",
    baseUrl: "fixed",
    key: "required",
    model: "gpt-5-mini",
    fastModel: "gpt-5-nano",
    keyUrl: "https://platform.openai.com/api-keys",
    experimental: false,
    personalOnly: false,
    embeddings: false,
  },
  {
    kind: "anthropic",
    name: "Anthropic",
    defaultBaseUrl: "https://api.anthropic.com/v1",
    baseUrl: "fixed",
    key: "required",
    model: "claude-sonnet-4-5",
    fastModel: "claude-haiku-4-5",
    keyUrl: "https://console.anthropic.com/settings/keys",
    experimental: false,
    personalOnly: false,
    embeddings: false,
  },
  {
    kind: "gemini",
    name: "Google Gemini",
    defaultBaseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
    baseUrl: "fixed",
    key: "required",
    model: "gemini-2.5-flash",
    fastModel: "gemini-2.5-flash-lite",
    keyUrl: "https://aistudio.google.com/apikey",
    experimental: false,
    personalOnly: false,
    embeddings: false,
  },
  {
    kind: "mistral",
    name: "Mistral",
    defaultBaseUrl: "https://api.mistral.ai/v1",
    baseUrl: "fixed",
    key: "required",
    model: "mistral-medium-latest",
    fastModel: "mistral-small-latest",
    keyUrl: "https://console.mistral.ai/api-keys",
    experimental: false,
    personalOnly: false,
    embeddings: false,
  },
  {
    kind: "openrouter",
    name: "OpenRouter",
    defaultBaseUrl: "https://openrouter.ai/api/v1",
    baseUrl: "optional",
    key: "required",
    model: "openai/gpt-5-mini",
    fastModel: "google/gemini-2.5-flash-lite",
    keyUrl: "https://openrouter.ai/settings/keys",
    experimental: false,
    personalOnly: false,
    embeddings: false,
  },
  {
    kind: "ollama",
    name: "Ollama",
    defaultBaseUrl: "http://localhost:11434",
    baseUrl: "required",
    key: "none",
    model: null,
    fastModel: null,
    keyUrl: null,
    experimental: false,
    personalOnly: false,
    embeddings: false,
  },
  {
    kind: "openaiCompatible",
    name: "OpenAI-compatible",
    defaultBaseUrl: null,
    baseUrl: "required",
    key: "optional",
    model: null,
    fastModel: null,
    keyUrl: null,
    experimental: false,
    personalOnly: false,
    embeddings: false,
  },
  {
    kind: "chatgpt",
    name: "ChatGPT (subscription)",
    defaultBaseUrl: null,
    baseUrl: "fixed",
    key: "login",
    model: "gpt-5",
    fastModel: "gpt-5-mini",
    keyUrl: null,
    experimental: true,
    personalOnly: true,
    embeddings: false,
  },
  {
    kind: "openaiEmbeddings",
    name: "OpenAI embeddings",
    defaultBaseUrl: "https://api.openai.com/v1",
    baseUrl: "optional",
    key: "required",
    model: "text-embedding-3-small",
    fastModel: null,
    keyUrl: "https://platform.openai.com/api-keys",
    experimental: false,
    personalOnly: false,
    embeddings: true,
  },
  {
    kind: "ollamaEmbeddings",
    name: "Ollama embeddings",
    defaultBaseUrl: null,
    baseUrl: "required",
    key: "none",
    model: "nomic-embed-text",
    fastModel: null,
    keyUrl: null,
    experimental: false,
    personalOnly: false,
    embeddings: true,
  },
  {
    kind: "embeddingsCompatible",
    name: "OpenAI-compatible embeddings",
    defaultBaseUrl: null,
    baseUrl: "required",
    key: "optional",
    model: null,
    fastModel: null,
    keyUrl: null,
    experimental: false,
    personalOnly: false,
    embeddings: true,
  },
];

const MODELS: Record<ProviderKind, string[]> = {
  openai: ["gpt-5", "gpt-5-mini", "gpt-5-nano", "gpt-4.1", "gpt-4.1-mini"],
  anthropic: ["claude-opus-4-1", "claude-sonnet-4-5", "claude-haiku-4-5"],
  gemini: ["gemini-2.5-pro", "gemini-2.5-flash", "gemini-2.5-flash-lite"],
  mistral: ["mistral-large-latest", "mistral-medium-latest", "mistral-small-latest"],
  openrouter: ["anthropic/claude-sonnet-4.5", "google/gemini-2.5-flash-lite", "openai/gpt-5-mini"],
  ollama: ["gemma3:12b", "llama3.2:3b", "llama3.3:70b", "qwen3:14b"],
  openaiCompatible: ["qwen3-32b", "mistral-small-3.2"],
  chatgpt: ["gpt-5", "gpt-5-codex", "gpt-5-mini"],
  openaiEmbeddings: ["text-embedding-3-small", "text-embedding-3-large"],
  ollamaEmbeddings: ["nomic-embed-text", "bge-m3"],
  embeddingsCompatible: ["bge-m3", "multilingual-e5-large"],
};

const kindOf = (kind: string) => KINDS.find((candidate) => candidate.kind === kind);

// What models cost, as the price lists would say: US dollars per million tokens in and out.
const LIST_PRICES: Record<string, [number, number]> = {
  "gpt-5": [1.25, 10],
  "gpt-5-mini": [0.25, 2],
  "gpt-5-nano": [0.05, 0.4],
  "mistral-medium-latest": [0.4, 2],
  "mistral-small-latest": [0.1, 0.3],
  "claude-haiku-4-5": [1, 5],
  "gemini-2.5-flash-lite": [0.1, 0.4],
};
/** The rest of the price sheet where the lists know more: thinking, the cache, fees, tiers. */
const LIST_EXTRAS: Record<string, Partial<Price>> = {
  "gpt-5": { supportsReasoning: true, maxOutputTokens: 128_000, cacheReadPerMillion: 0.125 },
  "gpt-5-mini": { supportsReasoning: true, maxOutputTokens: 128_000, cacheReadPerMillion: 0.025 },
  "gpt-5-nano": { supportsReasoning: true, maxOutputTokens: 128_000, cacheReadPerMillion: 0.005 },
  "claude-haiku-4-5": { maxOutputTokens: 64_000, cacheReadPerMillion: 0.1, cacheWritePerMillion: 1.25 },
  "gemini-2.5-flash-lite": {
    supportsReasoning: true,
    maxOutputTokens: 65_535,
    perImage: 0.0001,
    tiers: [
      {
        aboveTokens: 200_000,
        inputPerMillion: 0.2,
        outputPerMillion: 0.8,
        reasoningPerMillion: 0.8,
        cacheReadPerMillion: 0.05,
      },
    ],
  },
};
/** A whole price sheet from a price in and out, and what the lists know beyond. */
function sheet(input: number, output: number, source: Price["source"], extras: Partial<Price> = {}): Price {
  return {
    inputPerMillion: input,
    outputPerMillion: output,
    reasoningPerMillion: output,
    cacheReadPerMillion: input,
    cacheWritePerMillion: input,
    perRequest: 0,
    perImage: 0,
    webSearchPerQuery: 0,
    tiers: [],
    supportsReasoning: false,
    maxOutputTokens: null,
    ...extras,
    source,
  };
}

/** Units per euro, like the ECB's reference rates. */
const RATES: Record<string, number> = { EUR: 1, USD: 1.17, JPY: 172, CNY: 8.35 };

function priceOf(
  kind: ProviderKind,
  model: string | null | undefined,
  inputPrice: number | null | undefined,
  outputPrice: number | null | undefined,
  requestPrice?: number | null,
): Price | null {
  const name = model?.replace(/^[^/]+\//, "") ?? "";
  const listed = LIST_PRICES[name];
  const extras = LIST_EXTRAS[name] ?? {};
  const facts = { supportsReasoning: extras.supportsReasoning, maxOutputTokens: extras.maxOutputTokens };
  if (inputPrice != null || outputPrice != null || requestPrice != null) {
    // By hand: thinking as the answer, the cache as the prompt, no tiers; the model's facts stay.
    return sheet(inputPrice ?? listed?.[0] ?? 0, outputPrice ?? listed?.[1] ?? 0, "manual", {
      ...facts,
      perRequest: requestPrice ?? extras.perRequest ?? 0,
      perImage: extras.perImage ?? 0,
    });
  }
  if (kind === "ollama" || kind === "chatgpt") return sheet(0, 0, "free", facts);
  return listed ? sheet(listed[0], listed[1], "auto", extras) : null;
}

/** US dollars in `currency`, as the server answers `cost`. */
function costIn(usd: number | null | undefined, currency: string): Cost | null {
  if (usd == null) return null;
  const rate = RATES[currency];
  if (rate === undefined) return null;
  return { amount: currency === "USD" ? usd : (usd / RATES.USD!) * rate, currency, usd };
}

const currencyOf = (search: URLSearchParams) => search.get("currency") ?? "EUR";

let policy: AssistPolicy = {
  features: { compose: true, summarize: true, spamCheck: true, extractEvents: true, autoLabels: true },
  allowPersonal: true,
  allowPersonalPrivate: false,
  foreignMail: false,
};

/** Keys are kept only to answer `hasKey` and `keyHint`, as the real server does. */
const keys = new Map<number, string>();

const serverProviders: AdminProvider[] = [
  {
    id: 1,
    name: "Ollama im Keller",
    kind: "ollama",
    baseUrl: "http://192.0.2.10:11434",
    hasKey: false,
    keyHint: null,
    model: "llama3.3:70b",
    fastModel: "llama3.2:3b",
    enabled: true,
    access: "everyone",
    domains: [],
    people: [],
    features: [...FEATURES],
    requestsPerDay: null,
    tokensPerDay: null,
    inputPricePerMillion: null,
    outputPricePerMillion: null,
    pricePerRequest: null,
    showCostToUsers: true,
    price: null,
    createdAt: now() - 40 * 86_400,
  },
  {
    id: 2,
    name: "OpenAI (Team)",
    kind: "openai",
    baseUrl: null,
    hasKey: true,
    keyHint: "…a1b2",
    model: "gpt-5-mini",
    fastModel: "gpt-5-nano",
    enabled: true,
    access: "domains",
    domains: ["uwu.example"],
    people: [],
    features: ["compose", "summarize", "extractEvents"],
    requestsPerDay: 200,
    tokensPerDay: 400_000,
    inputPricePerMillion: null,
    outputPricePerMillion: null,
    pricePerRequest: null,
    showCostToUsers: false,
    price: null,
    createdAt: now() - 12 * 86_400,
  },
  {
    id: 3,
    name: "Gemma im Büro",
    kind: "openaiCompatible",
    baseUrl: "http://192.0.2.11:8080/v1",
    hasKey: false,
    keyHint: null,
    model: "gemma-3-4b-it",
    fastModel: null,
    enabled: false,
    access: "people",
    domains: [],
    people: [ME],
    features: ["autoLabels", "spamCheck"],
    requestsPerDay: null,
    tokensPerDay: null,
    inputPricePerMillion: null,
    outputPricePerMillion: null,
    pricePerRequest: null,
    showCostToUsers: false,
    price: null,
    createdAt: now() - 5 * 86_400,
  },
  {
    id: 4,
    name: "Ähnliche Mails",
    kind: "ollamaEmbeddings",
    baseUrl: "http://192.0.2.10:11434",
    hasKey: false,
    keyHint: null,
    model: "nomic-embed-text",
    fastModel: null,
    enabled: true,
    access: "everyone",
    domains: [],
    people: [],
    features: [],
    requestsPerDay: null,
    tokensPerDay: null,
    inputPricePerMillion: null,
    outputPricePerMillion: null,
    pricePerRequest: null,
    showCostToUsers: false,
    price: null,
    createdAt: now() - 2 * 86_400,
  },
];
keys.set(2, "sk-mock-a1b2");

interface OwnProvider {
  id: number;
  name: string;
  kind: ProviderKind;
  baseUrl: string | null;
  model: string | null;
  fastModel: string | null;
  connected: boolean;
  /** Polls of a ChatGPT login so far; null while none runs. */
  polls: number | null;
  expiresAt: number;
  inputPrice: number | null;
  outputPrice: number | null;
  requestPrice: number | null;
}

const ownProviders: OwnProvider[] = [
  {
    id: 11,
    name: "Mein Mistral",
    kind: "mistral",
    baseUrl: null,
    model: "mistral-medium-latest",
    fastModel: null,
    connected: true,
    polls: null,
    expiresAt: 0,
    inputPrice: null,
    outputPrice: null,
    requestPrice: null,
  },
];
keys.set(11, "mock-mistral-9f3c");
let nextId = 20;

const emptyChoices = (): Record<Feature, Choice | null> => ({
  compose: null,
  summarize: null,
  spamCheck: null,
  extractEvents: null,
  autoLabels: null,
});

const settings: Omit<AssistSettings, "effective"> = {
  default: { providerId: 1, model: null },
  features: { ...emptyChoices(), compose: { providerId: 11, model: null } },
  autoLabels: false,
  nonAiLabels: true,
  refineEvents: false,
  currency: null,
};

const LABEL_LIST: LabelSummary[] = [
  {
    id: 1,
    name: "Rechnungen",
    color: "#30a46c",
    detector: "invoice",
    hasRules: true,
    learnSenders: true,
    classifier: true,
    totalEmails: 48,
    unreadEmails: 2,
    examples: 21,
  },
  {
    id: 2,
    name: "Reisen",
    color: "#0090ff",
    detector: null,
    hasRules: false,
    learnSenders: true,
    classifier: true,
    totalEmails: 9,
    unreadEmails: 0,
    examples: 6,
  },
  {
    id: 3,
    name: "Verein",
    color: null,
    detector: null,
    hasRules: false,
    learnSenders: false,
    classifier: false,
    totalEmails: 3,
    unreadEmails: 1,
    examples: 0,
  },
];
const LABELS = LABEL_LIST.length;

const hint = (id: number) => {
  const key = keys.get(id);
  return key ? `…${key.slice(-4)}` : null;
};

/** Whether the pretend person may use a server provider: they are on uwu.example. */
function mayUse(provider: AdminProvider): boolean {
  if (!provider.enabled) return false;
  if (provider.access === "domains") return provider.domains.includes(ME.split("@")[1]!);
  if (provider.access === "people") return provider.people.includes(ME);
  return true;
}

function allowedFeatures(features: ProviderFeature[]): ProviderFeature[] {
  return features.filter((feature) =>
    feature === FOREIGN_MAIL ? Boolean(policy.foreignMail) : policy.features[feature],
  );
}

/** What the default model's name says about its size, as the server reads it ("gemma-3-4b-it": 4). */
function modelHint(provider: AdminProvider): ModelHint | null {
  const kind = kindOf(provider.kind);
  if (!kind || kind.embeddings) return null;
  const size = /(?:^|[^a-z0-9.])(\d+(?:\.\d+)?)b(?![a-z0-9])/i.exec(provider.model ?? kind.model ?? "");
  if (!size) return null;
  const billions = Number(size[1]);
  return { billions, small: billions < 7, recommended: ["Qwen3-8B", "Qwen3-14B", "gemma-3-12b-it"] };
}

function adminPrice(provider: AdminProvider): Price | null {
  return priceOf(
    provider.kind,
    provider.model ?? kindOf(provider.kind)?.model,
    provider.inputPricePerMillion,
    provider.outputPricePerMillion,
    provider.pricePerRequest,
  );
}

/** Whether the pretend person sees what a provider costs: their own always, the server's when shown. */
function costShown(providerId: number): boolean {
  if (ownProviders.some((provider) => provider.id === providerId)) return true;
  return serverProviders.some((provider) => provider.id === providerId && provider.showCostToUsers);
}

function accountProviders(): AssistProvider[] {
  // Embeddings providers serve no feature: people never see them.
  const server = serverProviders
    .filter((provider) => mayUse(provider) && !kindOf(provider.kind)?.embeddings)
    .map((provider): AssistProvider => ({
      id: provider.id,
      name: provider.name,
      kind: provider.kind,
      scope: "server",
      baseUrl: null,
      hasKey: provider.hasKey,
      keyHint: null,
      model: provider.model ?? kindOf(provider.kind)?.model ?? null,
      fastModel: provider.fastModel ?? kindOf(provider.kind)?.fastModel ?? null,
      features: allowedFeatures(provider.features),
      quota:
        provider.requestsPerDay === null && provider.tokensPerDay === null
          ? null
          : { requestsPerDay: provider.requestsPerDay, tokensPerDay: provider.tokensPerDay },
      experimental: false,
      connected: true,
      inputPricePerMillion: provider.showCostToUsers ? provider.inputPricePerMillion : null,
      outputPricePerMillion: provider.showCostToUsers ? provider.outputPricePerMillion : null,
      pricePerRequest: provider.showCostToUsers ? (provider.pricePerRequest ?? null) : null,
      price: provider.showCostToUsers ? adminPrice(provider) : null,
    }));
  const own = policy.allowPersonal
    ? ownProviders.map((provider): AssistProvider => {
        const kind = kindOf(provider.kind);
        return {
          id: provider.id,
          name: provider.name,
          kind: provider.kind,
          scope: "personal",
          baseUrl: provider.baseUrl,
          hasKey: keys.has(provider.id),
          keyHint: hint(provider.id),
          model: provider.model,
          fastModel: provider.fastModel,
          features: allowedFeatures([...FEATURES, FOREIGN_MAIL]),
          quota: null,
          experimental: kind?.experimental ?? false,
          connected:
            kind?.key === "login" ? provider.connected : kind?.key === "required" ? keys.has(provider.id) : true,
          inputPricePerMillion: provider.inputPrice,
          outputPricePerMillion: provider.outputPrice,
          pricePerRequest: provider.requestPrice,
          price: priceOf(
            provider.kind,
            provider.model ?? kind?.model,
            provider.inputPrice,
            provider.outputPrice,
            provider.requestPrice,
          ),
        };
      })
    : [];
  return [...server, ...own];
}

function effectiveSettings(): AssistSettings {
  const providers = accountProviders();
  const effective = Object.fromEntries(
    FEATURES.map((feature) => {
      const usable = providers.filter((provider) => provider.connected && provider.features.includes(feature));
      const pick = (choice: Choice | null) => (choice ? usable.find((p) => p.id === choice.providerId) : undefined);
      const choice = [settings.features[feature], settings.default].find((candidate) => pick(candidate));
      const provider =
        pick(choice ?? null) ??
        usable.find((candidate) => candidate.scope === "server") ??
        usable.find((candidate) => candidate.scope === "personal");
      if (!provider) return [feature, null];
      const model =
        choice?.model ??
        (feature === "compose" ? provider.model : (provider.fastModel ?? provider.model)) ??
        "(default)";
      const value: Effective = {
        providerId: provider.id,
        providerName: provider.name,
        model,
        scope: provider.scope,
      };
      return [feature, value];
    }),
  ) as Record<Feature, Effective | null>;
  return { ...settings, effective };
}

// A month of usage: most on the Ollama, some on the team key, a little on the own Mistral.
const PEOPLE = [ME, "leni@uwu.example", "mini@uwu.example", "ami@verein.example"];
const usage: (UsageRow & { costUsd: number | null })[] = [];
{
  let seed = 7;
  const random = () => {
    seed = (seed * 16807) % 2147483647;
    return seed / 2147483647;
  };
  for (let back = 0; back < 30; back++) {
    const day = new Date(Date.now() - back * 86_400_000).toISOString().slice(0, 10);
    for (const login of PEOPLE) {
      for (const feature of FEATURES) {
        if (random() < 0.45) continue;
        const providerId = login === ME && feature === "compose" ? 11 : random() < 0.7 ? 1 : 2;
        if (providerId === 2 && login === "ami@verein.example") continue;
        const requests = Math.max(1, Math.round(random() * (feature === "autoLabels" ? 30 : 8)));
        const inputTokens = requests * Math.round(800 + random() * 2400);
        const outputTokens = requests * Math.round(60 + random() * 400);
        // gpt-5-nano on the team key thinks first; the first days are from before thinking was kept.
        const reasoningTokens = providerId === 2 && back <= 24 ? requests * Math.round(200 + random() * 900) : 0;
        const cachedTokens = providerId === 2 ? Math.round(inputTokens * random() * 0.3) : 0;
        // The Ollama is free, the team key costs what gpt-5-nano costs, the own Mistral mistral-medium;
        // the first days are from before costs were kept.
        const perMillion = providerId === 1 ? [0, 0] : providerId === 2 ? [0.05, 0.4] : [0.4, 2];
        usage.push({
          day,
          login,
          providerId,
          providerName: providerId === 11 ? "Mein Mistral" : providerId === 1 ? "Ollama im Keller" : "OpenAI (Team)",
          feature,
          requests,
          inputTokens,
          outputTokens,
          reasoningTokens,
          cachedTokens,
          calls: requests,
          costUsd:
            back > 24 ? null : (inputTokens * perMillion[0]! + (outputTokens + reasoningTokens) * perMillion[1]!) / 1e6,
        });
      }
    }
  }
}

function today(currency = "EUR"): TodayUsage[] {
  const day = new Date().toISOString().slice(0, 10);
  return accountProviders().flatMap((provider) => {
    const rows = usage.filter((row) => row.day === day && row.login === ME && row.providerId === provider.id);
    if (rows.length === 0 && !provider.quota) return [];
    return [
      {
        providerId: provider.id,
        providerName: provider.name,
        requests: rows.reduce((sum, row) => sum + row.requests, 0),
        tokens: rows.reduce((sum, row) => sum + row.inputTokens + row.outputTokens + (row.reasoningTokens ?? 0), 0),
        requestsPerDay: provider.quota?.requestsPerDay ?? null,
        tokensPerDay: provider.quota?.tokensPerDay ?? null,
        cost: costShown(provider.id)
          ? costIn(
              rows.reduce((sum, row) => sum + (row.costUsd ?? 0), 0),
              currency,
            )
          : null,
      },
    ];
  });
}

function accountView(): AccountAssistView {
  const providers = accountProviders();
  const features = Object.fromEntries(
    FEATURES.map((feature) => [
      feature,
      policy.features[feature] &&
        providers.some((provider) => provider.connected && provider.features.includes(feature)),
    ]),
  ) as Record<Feature, boolean>;
  return {
    features,
    foreignMail: Boolean(policy.foreignMail) && Object.values(features).some(Boolean),
    mayAddProviders: policy.allowPersonal,
    mayUsePrivateAddresses: policy.allowPersonal && policy.allowPersonalPrivate,
    maxProviders: 10,
    providers,
    settings: effectiveSettings(),
    today: today(),
    kinds: KINDS,
    labels: LABELS,
    labelList: LABEL_LIST,
  };
}

/** What the server checks about an address; `privateOk` for the admin or when the policy allows it. */
function urlError(raw: unknown, privateOk: boolean): [number, unknown] | null {
  if (raw === undefined || raw === null || raw === "") return null;
  let url: URL;
  try {
    url = new URL(String(raw));
  } catch {
    return problem(409, "badProviderUrl");
  }
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password) {
    return problem(409, "badProviderUrl");
  }
  const host = url.hostname;
  const local =
    host === "localhost" ||
    host.endsWith(".localhost") ||
    /^(10|127)\./.test(host) ||
    /^192\.(168|0\.2)\./.test(host) ||
    /^172\.(1[6-9]|2\d|3[01])\./.test(host) ||
    host.startsWith("[fd") ||
    host === "[::1]";
  if (local && !privateOk) return problem(409, "privateAddress");
  if (!local && url.protocol === "http:") return problem(409, "plainHttpPublic");
  return null;
}

interface ProviderInput {
  name?: string;
  kind?: string;
  baseUrl?: string | null;
  apiKey?: string;
  model?: string | null;
  fastModel?: string | null;
  enabled?: boolean;
  access?: AdminProvider["access"];
  domains?: string[];
  people?: string[];
  features?: string[];
  requestsPerDay?: number | null;
  tokensPerDay?: number | null;
  inputPricePerMillion?: number | null;
  outputPricePerMillion?: number | null;
  pricePerRequest?: number | null;
  showCostToUsers?: boolean;
}

function inputError(input: ProviderInput, privateOk: boolean): [number, unknown] | null {
  if (input.name !== undefined && (input.name.trim() === "" || input.name.length > 60)) {
    return problem(409, "badProviderName");
  }
  if (input.features?.some((feature) => !(FEATURES as readonly string[]).includes(feature))) {
    return problem(409, "badFeature");
  }
  if (
    (input.access === "domains" && (input.domains ?? []).length === 0) ||
    (input.access === "people" && (input.people ?? []).length === 0)
  ) {
    return problem(409, "badAccess");
  }
  for (const limit of [input.requestsPerDay, input.tokensPerDay]) {
    if (limit !== undefined && limit !== null && (!Number.isInteger(limit) || limit < 1)) {
      return problem(409, "badQuota");
    }
  }
  for (const price of [input.inputPricePerMillion, input.outputPricePerMillion]) {
    if (price !== undefined && price !== null && !(price >= 0 && price <= 100_000)) {
      return problem(409, "badPrice");
    }
  }
  const perRequest = input.pricePerRequest;
  if (perRequest !== undefined && perRequest !== null && !(perRequest >= 0 && perRequest <= 100)) {
    return problem(409, "badPrice");
  }
  return urlError(input.baseUrl, privateOk);
}

function storeKey(id: number, apiKey: string | undefined) {
  if (apiKey === undefined) return;
  if (apiKey === "") keys.delete(id);
  else keys.set(id, apiKey);
}

function modelsOf(kind: ProviderKind, id: number, name: string, model: string | null, fastModel: string | null) {
  const info = kindOf(kind);
  if (info?.key === "required" && !keys.has(id)) return problem(409, "providerFailed", "No key is stored.");
  if ((keys.get(id) ?? "").includes("wrong")) {
    return problem(409, "providerFailed", "The provider refused the key (HTTP 401).");
  }
  if (name.toLowerCase().includes("unreachable")) {
    return problem(409, "providerFailed", "No answer within 20 seconds.");
  }
  return [
    200,
    {
      models: MODELS[kind].map((model) => ({ id: model, name: model })),
      model: model ?? info?.model ?? MODELS[kind][0] ?? null,
      fastModel: fastModel ?? info?.fastModel ?? null,
    },
  ] as [number, unknown];
}

const adminView = () => ({
  policy,
  providers: serverProviders.map((provider) => ({
    ...provider,
    hasKey: keys.has(provider.id),
    keyHint: hint(provider.id),
    price: adminPrice(provider),
    modelHint: modelHint(provider),
  })),
  kinds: KINDS,
  priceLists: { fetchedAt: now() - 3 * 3600, models: 1874, openrouterModels: 0, ratesDay: null },
});

const adminProvider = (id: number) => {
  const provider = serverProviders.find((candidate) => candidate.id === id);
  return provider
    ? {
        ...provider,
        hasKey: keys.has(id),
        keyHint: hint(id),
        price: adminPrice(provider),
        modelHint: modelHint(provider),
      }
    : undefined;
};

const accountProvider = (id: number) => accountProviders().find((provider) => provider.id === id);

export const assistMockRoutes: [string, RegExp, Handler][] = [
  // The admin's side.
  ["GET", /^\/api\/admin\/assist$/, () => [200, adminView()]],
  [
    "PUT",
    /^\/api\/admin\/assist\/policy$/,
    (body) => {
      const next = body as AssistPolicy;
      policy = {
        features: { ...policy.features, ...next.features },
        allowPersonal: Boolean(next.allowPersonal),
        allowPersonalPrivate: Boolean(next.allowPersonal && next.allowPersonalPrivate),
        foreignMail: Boolean(next.foreignMail),
      };
      return [200, policy];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/assist\/providers$/,
    (body) => {
      const input = body as ProviderInput;
      const kind = kindOf(input.kind ?? "");
      if (!kind || kind.personalOnly) return problem(409, "badProviderKind");
      if (kind.kind === "embeddingsCompatible" && !input.model) return problem(409, "badModel");
      const failed = inputError({ ...input, name: input.name ?? "" }, true);
      if (failed) return failed;
      if (kind.key === "required" && !input.apiKey) return problem(409, "badProviderKey");
      const id = nextId++;
      storeKey(id, input.apiKey);
      serverProviders.push({
        id,
        name: input.name!.trim(),
        kind: kind.kind,
        baseUrl: kind.baseUrl === "fixed" ? null : (input.baseUrl ?? kind.defaultBaseUrl),
        hasKey: keys.has(id),
        keyHint: hint(id),
        model: input.model ?? null,
        fastModel: input.fastModel ?? null,
        enabled: input.enabled ?? true,
        access: input.access ?? "everyone",
        domains: input.domains ?? [],
        people: input.people ?? [],
        features: kind.embeddings
          ? []
          : ((input.features as ProviderFeature[] | undefined) ?? [...FEATURES, FOREIGN_MAIL]),
        requestsPerDay: input.requestsPerDay ?? null,
        tokensPerDay: input.tokensPerDay ?? null,
        inputPricePerMillion: input.inputPricePerMillion ?? null,
        outputPricePerMillion: input.outputPricePerMillion ?? null,
        pricePerRequest: input.pricePerRequest ?? null,
        showCostToUsers: input.showCostToUsers ?? false,
        price: null,
        createdAt: now(),
      });
      return [201, adminProvider(id)];
    },
  ],
  [
    "PATCH",
    /^\/api\/admin\/assist\/providers\/(\d+)$/,
    (body, [id]) => {
      const provider = serverProviders.find((candidate) => candidate.id === Number(id));
      if (!provider) return problem(404, "notFound");
      const input = body as ProviderInput;
      const failed = inputError(input, true);
      if (failed) return failed;
      storeKey(provider.id, input.apiKey);
      // The kind stays what it was created as, and the key never goes into the provider itself.
      const rest = { ...input, name: input.name?.trim() ?? provider.name } as Partial<ProviderInput>;
      delete rest.apiKey;
      delete rest.kind;
      Object.assign(provider, rest);
      return [200, adminProvider(provider.id)];
    },
  ],
  [
    "DELETE",
    /^\/api\/admin\/assist\/providers\/(\d+)$/,
    (_body, [id]) => {
      const at = serverProviders.findIndex((candidate) => candidate.id === Number(id));
      if (at < 0) return problem(404, "notFound");
      serverProviders.splice(at, 1);
      keys.delete(Number(id));
      return [204, null];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/assist\/providers\/(\d+)\/models$/,
    (_body, [id]) => {
      const provider = serverProviders.find((candidate) => candidate.id === Number(id));
      if (!provider) return problem(404, "notFound");
      return modelsOf(provider.kind, provider.id, provider.name, provider.model, provider.fastModel);
    },
  ],
  [
    "GET",
    /^\/api\/admin\/assist\/usage$/,
    (_body, _params, search) => {
      const days = Number(search.get("days") ?? 30);
      const since = new Date(Date.now() - (days - 1) * 86_400_000).toISOString().slice(0, 10);
      const names = new Map<number, string>([
        ...serverProviders.map((provider) => [provider.id, provider.name] as const),
        ...ownProviders.map((provider) => [provider.id, provider.name] as const),
      ]);
      return [
        200,
        {
          days: usage
            .filter((row) => row.day >= since)
            .map(({ costUsd, ...row }) => ({
              ...row,
              providerName: names.get(row.providerId) ?? "(deleted)",
              cost: costIn(costUsd, currencyOf(search)),
            })),
        },
      ];
    },
  ],

  // The person's side.
  ["GET", /^\/api\/account\/assist$/, () => [200, accountView()]],
  [
    "POST",
    /^\/api\/account\/assist\/providers$/,
    (body) => {
      if (!policy.allowPersonal) return problem(403, "assistNotAllowed");
      if (ownProviders.length >= 10) return problem(409, "tooManyProviders");
      const input = body as ProviderInput;
      const kind = kindOf(input.kind ?? "");
      if (!kind || kind.embeddings) return problem(409, "badProviderKind");
      const failed = inputError({ ...input, name: input.name ?? "" }, policy.allowPersonalPrivate);
      if (failed) return failed;
      if (kind.key === "required" && !input.apiKey) return problem(409, "badProviderKey");
      const id = nextId++;
      storeKey(id, input.apiKey);
      ownProviders.push({
        id,
        name: input.name!.trim(),
        kind: kind.kind,
        baseUrl: kind.baseUrl === "fixed" ? null : (input.baseUrl ?? kind.defaultBaseUrl),
        model: input.model ?? null,
        fastModel: input.fastModel ?? null,
        connected: false,
        polls: null,
        expiresAt: 0,
        inputPrice: input.inputPricePerMillion ?? null,
        outputPrice: input.outputPricePerMillion ?? null,
        requestPrice: input.pricePerRequest ?? null,
      });
      return [201, accountProvider(id)];
    },
  ],
  [
    "PATCH",
    /^\/api\/account\/assist\/providers\/(\d+)$/,
    (body, [id]) => {
      const provider = ownProviders.find((candidate) => candidate.id === Number(id));
      if (!provider)
        return serverProviders.some((p) => p.id === Number(id))
          ? problem(403, "assistNotAllowed")
          : problem(404, "notFound");
      const input = body as ProviderInput;
      const failed = inputError(input, policy.allowPersonalPrivate);
      if (failed) return failed;
      storeKey(provider.id, input.apiKey);
      if (input.name !== undefined) provider.name = input.name.trim();
      if (input.baseUrl !== undefined) provider.baseUrl = input.baseUrl;
      if (input.model !== undefined) provider.model = input.model;
      if (input.fastModel !== undefined) provider.fastModel = input.fastModel;
      if (input.inputPricePerMillion !== undefined) provider.inputPrice = input.inputPricePerMillion;
      if (input.outputPricePerMillion !== undefined) provider.outputPrice = input.outputPricePerMillion;
      if (input.pricePerRequest !== undefined) provider.requestPrice = input.pricePerRequest;
      return [200, accountProvider(provider.id)];
    },
  ],
  [
    "DELETE",
    /^\/api\/account\/assist\/providers\/(\d+)$/,
    (_body, [id]) => {
      const at = ownProviders.findIndex((candidate) => candidate.id === Number(id));
      if (at < 0) return problem(404, "notFound");
      ownProviders.splice(at, 1);
      keys.delete(Number(id));
      // A choice that named it is dropped, as the server does.
      if (settings.default?.providerId === Number(id)) settings.default = null;
      for (const feature of FEATURES) {
        if (settings.features[feature]?.providerId === Number(id)) settings.features[feature] = null;
      }
      return [204, null];
    },
  ],
  [
    "POST",
    /^\/api\/account\/assist\/providers\/(\d+)\/models$/,
    (_body, [id]) => {
      const provider = accountProvider(Number(id));
      if (!provider) return problem(404, "notFound");
      const own = ownProviders.find((candidate) => candidate.id === provider.id);
      if (own?.kind === "chatgpt" && !own.connected) {
        return problem(409, "providerFailed", "Not signed in with ChatGPT yet.");
      }
      return modelsOf(provider.kind, provider.id, provider.name, provider.model, provider.fastModel);
    },
  ],
  [
    "POST",
    /^\/api\/account\/assist\/providers\/(\d+)\/chatgpt\/login$/,
    (_body, [id]) => {
      const provider = ownProviders.find((candidate) => candidate.id === Number(id));
      if (!provider) return problem(404, "notFound");
      if (provider.kind !== "chatgpt") return problem(409, "badProviderKind");
      provider.polls = 0;
      provider.expiresAt = now() + 15 * 60;
      const letters = "BCDFGHJKLMNPQRSTVWXZ";
      const pick = () => letters[Math.floor(Math.random() * letters.length)];
      const code = `${Array.from({ length: 4 }, pick).join("")}-${Array.from({ length: 4 }, pick).join("")}`;
      return [
        200,
        {
          userCode: code,
          // The real one is OpenAI's page; the pretend one must not send anybody anywhere real.
          verificationUri: "https://auth.example.com/codex/device",
          interval: 2,
          expiresAt: provider.expiresAt,
        },
      ];
    },
  ],
  [
    "POST",
    /^\/api\/account\/assist\/providers\/(\d+)\/chatgpt\/poll$/,
    (_body, [id]) => {
      const provider = ownProviders.find((candidate) => candidate.id === Number(id));
      if (!provider) return problem(404, "notFound");
      if (provider.polls === null) return problem(409, "chatgptNotStarted");
      if (now() >= provider.expiresAt) {
        provider.polls = null;
        return [200, { status: "expired" }];
      }
      provider.polls += 1;
      if (provider.polls < 3) return [200, { status: "pending" }];
      provider.polls = null;
      if (provider.name.toLowerCase().includes("denied")) {
        return [200, { status: "failed", description: "The sign-in was declined at OpenAI." }];
      }
      provider.connected = true;
      return [200, { status: "connected" }];
    },
  ],
  [
    "PUT",
    /^\/api\/account\/assist\/settings$/,
    (body) => {
      const input = body as Partial<Omit<AssistSettings, "effective">>;
      const providers = accountProviders();
      const known = (choice: Choice | null | undefined) =>
        !choice || providers.some((provider) => provider.id === choice.providerId);
      if (!known(input.default) || !Object.values(input.features ?? {}).every(known)) {
        return problem(409, "badProvider", "That provider cannot be used.");
      }
      if (input.default !== undefined) settings.default = input.default;
      if (input.features) {
        for (const feature of FEATURES) {
          if (feature in input.features) settings.features[feature] = input.features[feature] ?? null;
        }
      }
      if (input.autoLabels !== undefined) settings.autoLabels = input.autoLabels;
      if (input.nonAiLabels !== undefined) settings.nonAiLabels = input.nonAiLabels;
      if (input.refineEvents !== undefined) settings.refineEvents = input.refineEvents;
      if (input.currency !== undefined) {
        if (input.currency !== null && input.currency !== "EUR" && input.currency !== "USD") {
          return problem(409, "badCurrency");
        }
        settings.currency = input.currency;
      }
      return [200, effectiveSettings()];
    },
  ],
  [
    "GET",
    /^\/api\/account\/assist\/usage$/,
    (_body, _params, search) => {
      const days = Number(search.get("days") ?? 30);
      const since = new Date(Date.now() - (days - 1) * 86_400_000).toISOString().slice(0, 10);
      return [
        200,
        {
          days: usage
            .filter((row) => row.login === ME && row.day >= since)
            .map(({ login: _login, costUsd, ...row }) => ({
              ...row,
              cost: costShown(row.providerId) ? costIn(costUsd, currencyOf(search)) : null,
            })),
          today: today(currencyOf(search)),
        },
      ];
    },
  ],
];
