import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SignatureOverview } from "./signatures";

const api = vi.fn();
vi.mock("@/lib/api", () => ({ api: (...args: unknown[]) => api(...args) }));
vi.mock("@/i18n", () => ({
  useT: () => ({
    t: (key: string, options?: Record<string, unknown>) => (options ? `${key} ${JSON.stringify(options)}` : key),
    i18n: { language: "en" },
  }),
}));

const { DomainSignatures } = await import("./DomainSignatures");

const overview: SignatureOverview = {
  state: "7",
  allDomains: null,
  domains: [
    { domain: "example.net", addressCount: 1, signature: { text: "Net", html: "" }, company: null, source: "domain" },
    {
      domain: "example.org",
      addressCount: 2,
      signature: null,
      company: { mode: "footer", text: "Beispiel GmbH {name}", html: "" },
      source: "none",
    },
  ],
  identities: [
    {
      id: 1,
      name: "Mini",
      email: "mini@example.net",
      domain: "example.net",
      signature: null,
      effective: { text: "Net", html: "" },
      source: "domain",
    },
    {
      id: 2,
      name: "Mini",
      email: "mini@example.org",
      domain: "example.org",
      signature: null,
      effective: { text: "", html: "" },
      source: "none",
    },
    {
      id: 3,
      name: "Info",
      email: "info@example.org",
      domain: "example.org",
      signature: { text: "Info", html: "" },
      effective: { text: "Info", html: "" },
      source: "identity",
    },
  ],
};

function show() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <DomainSignatures />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  api.mockReset();
  api.mockImplementation(() => Promise.resolve(overview));
});
afterEach(cleanup);

describe("signatures per domain", () => {
  it("lists the domains with their address count and edits one signature for a domain", async () => {
    show();
    const select = (await screen.findByLabelText("mailbox.signatures.domain")) as HTMLSelectElement;
    expect([...select.options].map((option) => option.textContent)).toEqual([
      'mailbox.signatures.domainOption {"domain":"example.net","count":1}',
      'mailbox.signatures.domainOption {"domain":"example.org","count":2}',
    ]);
    expect((screen.getByTestId("domain-text") as HTMLTextAreaElement).value).toBe("Net");

    fireEvent.change(select, { target: { value: "example.org" } });
    // The company footer is shown with the placeholders filled for the first address.
    expect(await screen.findByText("Beispiel GmbH Mini")).toBeTruthy();
    // One address has its own signature: the exceptions say so.
    expect(screen.getByText('mailbox.signatures.exceptions {"count":1}')).toBeTruthy();

    fireEvent.change(screen.getByTestId("domain-text"), { target: { value: "Gruß, {name}" } });
    fireEvent.click(screen.getAllByText("mailbox.signatures.save")[0]!);
    await waitFor(() => expect(api).toHaveBeenCalledWith("/api/account/signatures", expect.anything()));
    const [, options] = api.mock.calls.find(([, options]) => options?.method === "PUT")!;
    expect(options.body).toEqual({ domains: { "example.org": { text: "Gruß, {name}", html: "" } } });
  });

  it("applies the signature to all domains and drops the domains' own ones", async () => {
    show();
    await screen.findByTestId("domain-text");
    fireEvent.click(screen.getByLabelText("mailbox.signatures.allDomains"));
    fireEvent.change(screen.getByTestId("domain-text"), { target: { value: "Für alle" } });
    fireEvent.click(screen.getByText("mailbox.signatures.save"));
    await waitFor(() => expect(api.mock.calls.some(([, options]) => options?.method === "PUT")).toBe(true));
    const [, options] = api.mock.calls.find(([, options]) => options?.method === "PUT")!;
    expect(options.body).toEqual({ domains: { "*": { text: "Für alle", html: "" }, "example.net": null } });
  });

  it("gives a single address its own signature and takes it back", async () => {
    show();
    const select = await screen.findByLabelText("mailbox.signatures.domain");
    fireEvent.change(select, { target: { value: "example.org" } });
    await screen.findByText("mini@example.org", { exact: false });
    fireEvent.click(screen.getByText("mailbox.signatures.override"));
    fireEvent.change(screen.getByTestId("identity-2-text"), { target: { value: "Nur Mini" } });
    const saves = screen.getAllByText("mailbox.signatures.save");
    fireEvent.click(saves[saves.length - 2]!);
    await waitFor(() => expect(api.mock.calls.some(([, options]) => options?.method === "PUT")).toBe(true));
    let [, options] = api.mock.calls.find(([, options]) => options?.method === "PUT")!;
    expect(options.body).toEqual({ identities: { "2": { text: "Nur Mini", html: "" } } });

    api.mockClear();
    fireEvent.click(screen.getAllByText("mailbox.signatures.backToDomain").at(-1)!);
    await waitFor(() => expect(api.mock.calls.some(([, options]) => options?.method === "PUT")).toBe(true));
    [, options] = api.mock.calls.find(([, options]) => options?.method === "PUT")!;
    expect(options.body).toEqual({ identities: { "3": null } });
  });
});
