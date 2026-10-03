import { describe, expect, it } from "vitest";
import {
  adminKinds,
  byDay,
  byFeature,
  byPerson,
  changeKind,
  currencyFor,
  draftOf,
  emptyDraft,
  FEATURES,
  FOREIGN_MAIL,
  formatCost,
  loginReducer,
  normalizeChip,
  parseLimit,
  parsePrice,
  personalKinds,
  hasThinking,
  isEmbeddings,
  listedFeatures,
  MAX_PRICE_PER_REQUEST,
  pollSeconds,
  providerBody,
  share,
  smallModelHint,
  urlProblem,
  usageSum,
  validateDraft,
  type AdminProvider,
  type KindInfo,
  type LoginState,
  type UsageRow,
} from "./model";

const kind = (over: Partial<KindInfo>): KindInfo => ({
  kind: "openai",
  name: "OpenAI",
  defaultBaseUrl: "https://api.openai.com/v1",
  baseUrl: "fixed",
  key: "required",
  model: "gpt-5-mini",
  fastModel: "gpt-5-nano",
  keyUrl: null,
  experimental: false,
  personalOnly: false,
  embeddings: false,
  ...over,
});

const OPENAI = kind({});
const OLLAMA = kind({
  kind: "ollama",
  name: "Ollama",
  defaultBaseUrl: "http://localhost:11434",
  baseUrl: "required",
  key: "none",
});
const COMPATIBLE = kind({
  kind: "openaiCompatible",
  name: "OpenAI-compatible",
  defaultBaseUrl: null,
  baseUrl: "required",
  key: "optional",
});
const OPENROUTER = kind({ kind: "openrouter", name: "OpenRouter", baseUrl: "optional" });
const CHATGPT = kind({ kind: "chatgpt", name: "ChatGPT", key: "login", experimental: true, personalOnly: true });
const OPENAI_EMBEDDINGS = kind({
  kind: "openaiEmbeddings",
  name: "OpenAI embeddings",
  baseUrl: "optional",
  model: "text-embedding-3-small",
  fastModel: null,
  embeddings: true,
});
const EMBEDDINGS_COMPATIBLE = kind({
  kind: "embeddingsCompatible",
  name: "OpenAI-compatible embeddings",
  defaultBaseUrl: null,
  baseUrl: "required",
  key: "optional",
  model: null,
  fastModel: null,
  embeddings: true,
});
const ALL_KINDS = [OPENAI, OLLAMA, COMPATIBLE, OPENROUTER, CHATGPT, OPENAI_EMBEDDINGS, EMBEDDINGS_COMPATIBLE];

describe("parseLimit", () => {
  it("reads empty as no limit", () => {
    expect(parseLimit("")).toBeNull();
    expect(parseLimit("   ")).toBeNull();
  });

  it("reads thousands separators and suffixes", () => {
    expect(parseLimit("200")).toBe(200);
    expect(parseLimit("100.000")).toBe(100_000);
    expect(parseLimit("100,000")).toBe(100_000);
    expect(parseLimit("100 000")).toBe(100_000);
    expect(parseLimit("100k")).toBe(100_000);
    expect(parseLimit("1.5k")).toBe(1_500);
    expect(parseLimit("1,5 k")).toBe(1_500);
    expect(parseLimit("2M")).toBe(2_000_000);
  });

  it("refuses what is not a whole number above zero", () => {
    expect(parseLimit("0")).toBe("invalid");
    expect(parseLimit("-5")).toBe("invalid");
    expect(parseLimit("abc")).toBe("invalid");
    expect(parseLimit("1.0005k")).toBe("invalid");
    expect(parseLimit("12x")).toBe("invalid");
  });
});

describe("share", () => {
  it("is null without a limit and stays between 0 and 1", () => {
    expect(share(5, null)).toBeNull();
    expect(share(5, 10)).toBe(0.5);
    expect(share(50, 10)).toBe(1);
  });
});

describe("the provider form", () => {
  it("checks name, address and key", () => {
    const draft = { ...emptyDraft(COMPATIBLE), name: " " };
    expect(validateDraft(draft, COMPATIBLE, { hasKey: false, admin: false })).toEqual({
      name: "nameRequired",
      baseUrl: "urlRequired",
    });
    expect(validateDraft({ ...draft, name: "x".repeat(61) }, COMPATIBLE, { hasKey: false, admin: false }).name).toBe(
      "nameTooLong",
    );
    expect(
      validateDraft({ ...draft, name: "Mine", baseUrl: "ftp://192.0.2.10" }, COMPATIBLE, {
        hasKey: false,
        admin: false,
      }),
    ).toEqual({ baseUrl: "urlInvalid" });
    expect(validateDraft(emptyDraft(OPENAI), OPENAI, { hasKey: false, admin: false })).toEqual({
      apiKey: "keyRequired",
    });
    // A stored key counts, unless it is being taken away.
    expect(validateDraft(emptyDraft(OPENAI), OPENAI, { hasKey: true, admin: false })).toEqual({});
    expect(validateDraft({ ...emptyDraft(OPENAI), removeKey: true }, OPENAI, { hasKey: true, admin: false })).toEqual({
      apiKey: "keyRequired",
    });
    // An optional address may stay empty: the kind's own is used.
    expect(validateDraft(emptyDraft(OPENROUTER), OPENROUTER, { hasKey: true, admin: false })).toEqual({});
    const fee = (requestPrice: string) =>
      validateDraft({ ...emptyDraft(OPENROUTER), requestPrice }, OPENROUTER, { hasKey: true, admin: false });
    expect(fee("0.01")).toEqual({});
    expect(fee("250")).toEqual({ requestPrice: "requestPriceInvalid" });
  });

  it("checks what only the admin sets", () => {
    const draft = {
      ...emptyDraft(OLLAMA),
      access: "domains" as const,
      features: [],
      requestsPerDay: "lots",
      tokensPerDay: "50k",
    };
    expect(validateDraft(draft, OLLAMA, { hasKey: false, admin: true })).toEqual({
      access: "domainsRequired",
      features: "featuresRequired",
      requestsPerDay: "limitInvalid",
    });
    expect(validateDraft({ ...draft, access: "people" }, OLLAMA, { hasKey: false, admin: false })).toEqual({});
  });

  it("counts mail of other accounts as no feature of its own", () => {
    const draft = emptyDraft(OLLAMA);
    expect(draft.features).toContain(FOREIGN_MAIL);
    const foreignOnly = { ...draft, features: [FOREIGN_MAIL] };
    expect(validateDraft(foreignOnly, OLLAMA, { hasKey: false, admin: true })).toEqual({
      features: "featuresRequired",
    });
    const picked = { ...draft, features: [FOREIGN_MAIL, "summarize" as const] };
    expect(providerBody(picked, OLLAMA, { create: true, admin: true }).features).toEqual(["summarize", FOREIGN_MAIL]);
  });

  it("summarizes a provider's features without the switch for other accounts", () => {
    // A provider migrated to 0.21 has every feature plus foreignMail: still "all features".
    expect(listedFeatures([...FEATURES, FOREIGN_MAIL])).toBe("all");
    expect(listedFeatures([...FEATURES])).toBe("all");
    expect(listedFeatures([FOREIGN_MAIL, "autoLabels", "summarize"])).toEqual(["summarize", "autoLabels"]);
  });

  it("finds logins in addresses", () => {
    expect(urlProblem("https://user:secret@api.example.com/v1")).toBe("urlLogin");
    expect(urlProblem("http://192.0.2.10:11434")).toBeNull();
    expect(urlProblem("192.0.2.10:11434")).toBe("urlInvalid");
  });

  it("keeps the stored key unless a new one is typed or it is removed", () => {
    const draft = draftOf({
      id: 3,
      name: "Mine",
      kind: "openaiCompatible",
      scope: "personal",
      baseUrl: "https://llm.example.net/v1",
      hasKey: true,
      keyHint: "…a1b2",
      model: "qwen3",
      fastModel: null,
      features: ["compose"],
      quota: null,
      experimental: false,
      connected: true,
    });
    const context = { create: false, admin: false };
    expect(providerBody(draft, COMPATIBLE, context)).toEqual({
      name: "Mine",
      baseUrl: "https://llm.example.net/v1",
      model: "qwen3",
      fastModel: null,
      inputPricePerMillion: null,
      outputPricePerMillion: null,
      pricePerRequest: null,
    });
    expect(providerBody({ ...draft, inputPrice: "0,15" }, COMPATIBLE, context).inputPricePerMillion).toBe(0.15);
    expect(providerBody({ ...draft, requestPrice: "0,005" }, COMPATIBLE, context).pricePerRequest).toBe(0.005);
    expect(providerBody({ ...draft, removeKey: true }, COMPATIBLE, context).apiKey).toBe("");
    expect(providerBody({ ...draft, apiKey: " sk-new " }, COMPATIBLE, context).apiKey).toBe("sk-new");
    // Clearing the address of an existing provider says so.
    expect(providerBody({ ...draft, baseUrl: "" }, COMPATIBLE, context).baseUrl).toBeNull();
  });

  it("sends kind only on create and nothing a kind does not use", () => {
    const body = providerBody({ ...emptyDraft(CHATGPT), apiKey: "stray" }, CHATGPT, { create: true, admin: false });
    expect(body).toEqual({
      name: "ChatGPT",
      kind: "chatgpt",
      model: null,
      fastModel: null,
      inputPricePerMillion: null,
      outputPricePerMillion: null,
      pricePerRequest: null,
    });
  });

  it("builds the admin's fields", () => {
    const admin: AdminProvider = {
      id: 1,
      name: "Ollama",
      kind: "ollama",
      baseUrl: "http://192.0.2.10:11434",
      hasKey: false,
      keyHint: null,
      model: "llama3.3",
      fastModel: "llama3.2",
      enabled: true,
      access: "people",
      domains: ["uwu.example"],
      people: ["leni@uwu.example"],
      features: ["summarize", "compose"],
      requestsPerDay: 200,
      tokensPerDay: null,
      inputPricePerMillion: 0.5,
      outputPricePerMillion: null,
      showCostToUsers: true,
      price: null,
      createdAt: 0,
    };
    const draft = { ...draftOf(admin), tokensPerDay: "1M" };
    expect(draft.requestsPerDay).toBe("200");
    expect([draft.inputPrice, draft.outputPrice, draft.showCostToUsers]).toEqual(["0.5", "", true]);
    expect(providerBody(draft, OLLAMA, { create: false, admin: true })).toMatchObject({
      access: "people",
      domains: [],
      people: ["leni@uwu.example"],
      features: ["compose", "summarize"],
      requestsPerDay: 200,
      tokensPerDay: 1_000_000,
      inputPricePerMillion: 0.5,
      outputPricePerMillion: null,
      showCostToUsers: true,
    });
    expect(validateDraft({ ...draft, outputPrice: "-1" }, OLLAMA, { hasKey: false, admin: true })).toEqual({
      outputPrice: "priceInvalid",
    });
  });

  it("lets name and address follow the kind until someone types their own", () => {
    let draft = emptyDraft(OPENAI);
    draft = changeKind(draft, OPENAI, OLLAMA);
    expect(draft).toMatchObject({ kind: "ollama", name: "Ollama", baseUrl: "http://localhost:11434" });
    draft = changeKind({ ...draft, name: "Keller-Ollama", baseUrl: "http://192.0.2.10:11434" }, OLLAMA, COMPATIBLE);
    expect(draft).toMatchObject({ name: "Keller-Ollama", baseUrl: "http://192.0.2.10:11434" });
    draft = changeKind({ ...draft, apiKey: "sk" }, COMPATIBLE, OPENAI);
    expect(draft).toMatchObject({ baseUrl: "", apiKey: "sk" });
  });

  it("offers embeddings kinds to the admin only, and kinds only people may add to people only", () => {
    expect(personalKinds(ALL_KINDS).map((info) => info.kind)).toEqual([
      "openai",
      "ollama",
      "openaiCompatible",
      "openrouter",
      "chatgpt",
    ]);
    expect(adminKinds(ALL_KINDS).map((info) => info.kind)).toEqual([
      "openai",
      "ollama",
      "openaiCompatible",
      "openrouter",
      "openaiEmbeddings",
      "embeddingsCompatible",
    ]);
    expect([isEmbeddings(OPENAI_EMBEDDINGS), isEmbeddings(OPENAI), isEmbeddings(undefined)]).toEqual([
      true,
      false,
      false,
    ]);
  });

  it("asks no features of an embeddings provider and sends neither features nor a fast model", () => {
    const admin = { hasKey: true, admin: true };
    const draft = { ...emptyDraft(OPENAI_EMBEDDINGS), features: [], fastModel: "stray" };
    expect(validateDraft(draft, OPENAI_EMBEDDINGS, admin)).toEqual({});
    const body = providerBody({ ...draft, apiKey: "sk-test" }, OPENAI_EMBEDDINGS, { create: true, admin: true });
    expect(body).not.toHaveProperty("features");
    expect(body).not.toHaveProperty("fastModel");
    expect(body).toMatchObject({ kind: "openaiEmbeddings", apiKey: "sk-test", model: null, access: "everyone" });
  });

  it("needs a model for the generic embeddings kind", () => {
    const admin = { hasKey: false, admin: true };
    const draft = { ...emptyDraft(EMBEDDINGS_COMPATIBLE), baseUrl: "http://192.0.2.10:8081/v1" };
    expect(validateDraft(draft, EMBEDDINGS_COMPATIBLE, admin)).toEqual({ model: "embeddingsModelRequired" });
    expect(validateDraft({ ...draft, model: " bge-m3 " }, EMBEDDINGS_COMPATIBLE, admin)).toEqual({});
    // The presets have a default model of their own.
    expect(
      validateDraft({ ...emptyDraft(OPENAI_EMBEDDINGS) }, OPENAI_EMBEDDINGS, { hasKey: true, admin: true }),
    ).toEqual({});
    // Chat kinds keep their default model too, even the generic one.
    expect(
      validateDraft({ ...emptyDraft(COMPATIBLE), baseUrl: "http://192.0.2.10:8080/v1" }, COMPATIBLE, admin),
    ).toEqual({});
  });

  it("warns about small models only", () => {
    const recommended = ["Qwen3-8B", "Qwen3-14B", "gemma-3-12b-it"];
    expect(smallModelHint({ modelHint: { billions: 4, small: true, recommended } })).toEqual({
      billions: 4,
      small: true,
      recommended,
    });
    expect(smallModelHint({ modelHint: { billions: 14, small: false, recommended } })).toBeNull();
    expect(smallModelHint({ modelHint: null })).toBeNull();
    expect(smallModelHint({})).toBeNull();
  });

  it("normalizes chips", () => {
    expect(normalizeChip(" @UWU.example ", [])).toBe("uwu.example");
    expect(normalizeChip("uwu.example", ["uwu.example"])).toBeNull();
    expect(normalizeChip("  ", [])).toBeNull();
  });
});

describe("the ChatGPT device login", () => {
  const start = {
    userCode: "ABCD-1234",
    verificationUri: "https://auth.openai.com/codex/device",
    interval: 5,
    expiresAt: 1000,
  };
  const waiting = (): LoginState =>
    loginReducer(loginReducer({ phase: "idle" }, { type: "start" }), { type: "started", answer: start });

  it("waits until the login is confirmed", () => {
    let state = waiting();
    expect(state).toMatchObject({ phase: "waiting", userCode: "ABCD-1234", interval: 5 });
    state = loginReducer(state, { type: "polled", answer: { status: "pending" } });
    expect(state.phase).toBe("waiting");
    state = loginReducer(state, { type: "polled", answer: { status: "connected" } });
    expect(state).toEqual({ phase: "connected" });
  });

  it("ends on expiry, failure or a network error", () => {
    expect(loginReducer(waiting(), { type: "polled", answer: { status: "expired" } })).toEqual({ phase: "expired" });
    expect(loginReducer(waiting(), { type: "polled", answer: { status: "failed", description: "denied" } })).toEqual({
      phase: "failed",
      description: "denied",
    });
    expect(loginReducer(waiting(), { type: "tick", now: 999 }).phase).toBe("waiting");
    expect(loginReducer(waiting(), { type: "tick", now: 1000 })).toEqual({ phase: "expired" });
    expect(loginReducer(waiting(), { type: "error", description: null })).toEqual({
      phase: "failed",
      description: null,
    });
  });

  it("ignores late answers and double starts", () => {
    const cancelled = loginReducer(waiting(), { type: "cancel" });
    expect(loginReducer(cancelled, { type: "polled", answer: { status: "connected" } })).toEqual({ phase: "idle" });
    expect(loginReducer(cancelled, { type: "started", answer: start })).toEqual({ phase: "idle" });
    const state = waiting();
    expect(loginReducer(state, { type: "start" })).toBe(state);
    expect(loginReducer({ phase: "connected" }, { type: "error", description: "x" })).toEqual({ phase: "connected" });
  });

  it("polls at a sane pace", () => {
    expect(pollSeconds(5)).toBe(5);
    expect(pollSeconds(0)).toBe(1);
    expect(pollSeconds(600)).toBe(30);
    expect(pollSeconds(Number.NaN)).toBe(5);
  });
});

describe("usage", () => {
  const row = (day: string, login: string, requests: number, feature: UsageRow["feature"] = "compose"): UsageRow => ({
    day,
    login,
    providerId: 1,
    providerName: "OpenAI",
    feature,
    requests,
    inputTokens: requests * 100,
    outputTokens: requests * 10,
  });
  const rows = [
    row("2026-09-28", "leni@uwu.example", 2),
    row("2026-09-29", "leni@uwu.example", 1),
    row("2026-09-29", "mini@uwu.example", 4),
    row("2026-09-29", "leni@uwu.example", 3, "summarize"),
  ];

  it("adds up everything", () => {
    expect(usageSum(rows)).toEqual({
      requests: 10,
      inputTokens: 1000,
      outputTokens: 100,
      reasoningTokens: 0,
      amount: null,
    });
    expect(hasThinking(rows)).toBe(false);
    const thought = rows.map((entry, index) => (index === 2 ? { ...entry, reasoningTokens: 640 } : entry));
    expect(usageSum(thought).reasoningTokens).toBe(640);
    expect(hasThinking(thought)).toBe(true);
    const priced = rows.map((entry, index) => ({
      ...entry,
      cost: index === 0 ? null : { amount: 0.25, currency: "EUR", usd: 0.3 },
    }));
    expect(usageSum(priced).amount).toBe(0.75);
    expect(byDay(priced).map((group) => group.amount)).toEqual([0.75, null]);
  });

  it("groups per day, newest first", () => {
    expect(byDay(rows).map((group) => [group.day, group.requests, group.inputTokens])).toEqual([
      ["2026-09-29", 8, 800],
      ["2026-09-28", 2, 200],
    ]);
  });

  it("groups per feature in the usual order", () => {
    expect(byFeature(rows).map((group) => [group.feature, group.requests])).toEqual([
      ["compose", 7],
      ["summarize", 3],
    ]);
  });

  it("groups per person", () => {
    expect(byPerson(rows).map((group) => [group.login, group.requests])).toEqual([
      ["leni@uwu.example", 6],
      ["mini@uwu.example", 4],
    ]);
  });
});

describe("costs", () => {
  it("picks the currency by the language", () => {
    expect(currencyFor("ja")).toBe("JPY");
    expect(currencyFor("zh-CN")).toBe("CNY");
    expect(currencyFor("de", "USD")).toBe("EUR");
    expect(currencyFor("en")).toBe("EUR");
    expect(currencyFor("en-US", "USD")).toBe("USD");
  });

  it("writes small amounts with enough digits", () => {
    expect(formatCost(1.5, "EUR", "en")).toBe("€1.50");
    expect(formatCost(0.0023, "EUR", "en")).toBe("€0.0023");
    expect(formatCost(0.0000004, "USD", "en")).toBe("< $0.0001");
    expect(formatCost(0, "EUR", "en")).toBe("€0");
    expect(formatCost(0.5, "JPY", "en")).toBe("¥0.5");
    expect(formatCost(12, "JPY", "en")).toBe("¥12");
  });

  it("reads prices per million tokens", () => {
    expect(parsePrice("")).toBeNull();
    expect(parsePrice("0,15")).toBe(0.15);
    expect(parsePrice("2.5")).toBe(2.5);
    expect(parsePrice("-1")).toBe("invalid");
    expect(parsePrice("1e3")).toBe("invalid");
    expect(parsePrice("100001")).toBe("invalid");
    expect(parsePrice("0.005", MAX_PRICE_PER_REQUEST)).toBe(0.005);
    expect(parsePrice("101", MAX_PRICE_PER_REQUEST)).toBe("invalid");
  });
});
