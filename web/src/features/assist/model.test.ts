import { describe, expect, it } from "vitest";
import {
  byDay,
  byFeature,
  byPerson,
  changeKind,
  draftOf,
  emptyDraft,
  loginReducer,
  normalizeChip,
  parseLimit,
  pollSeconds,
  providerBody,
  share,
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
    });
    expect(providerBody({ ...draft, removeKey: true }, COMPATIBLE, context).apiKey).toBe("");
    expect(providerBody({ ...draft, apiKey: " sk-new " }, COMPATIBLE, context).apiKey).toBe("sk-new");
    // Clearing the address of an existing provider says so.
    expect(providerBody({ ...draft, baseUrl: "" }, COMPATIBLE, context).baseUrl).toBeNull();
  });

  it("sends kind only on create and nothing a kind does not use", () => {
    const body = providerBody({ ...emptyDraft(CHATGPT), apiKey: "stray" }, CHATGPT, { create: true, admin: false });
    expect(body).toEqual({ name: "ChatGPT", kind: "chatgpt", model: null, fastModel: null });
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
      createdAt: 0,
    };
    const draft = { ...draftOf(admin), tokensPerDay: "1M" };
    expect(draft.requestsPerDay).toBe("200");
    expect(providerBody(draft, OLLAMA, { create: false, admin: true })).toMatchObject({
      access: "people",
      domains: [],
      people: ["leni@uwu.example"],
      features: ["compose", "summarize"],
      requestsPerDay: 200,
      tokensPerDay: 1_000_000,
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
    expect(usageSum(rows)).toEqual({ requests: 10, inputTokens: 1000, outputTokens: 100 });
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
