import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.fn();
let preferences: Record<string, unknown> = {};
vi.mock("@/lib/api", () => ({ api: (...args: unknown[]) => api(...args) }));
vi.mock("@/features/session/session", () => ({ useSession: () => ({ data: { preferences } }) }));
vi.mock("@/lib/errors", () => ({ useErrorText: () => (error: unknown) => String(error) }));
vi.mock("@/i18n", () => ({ useT: () => ({ t: (key: string) => key, i18n: { language: "en" } }) }));

const { CodesPage } = await import("./CodesPage");

function show() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <CodesPage />
    </QueryClientProvider>,
  );
}

const radio = (value: string) => screen.getByRole("radio", { name: `codes.cleanup.options.${value}` });

beforeEach(() => {
  api.mockReset();
  api.mockImplementation(() => Promise.resolve({}));
  preferences = {};
});
afterEach(cleanup);

describe("CodesPage", () => {
  it("starts with the server's defaults and saves each change as a preference", async () => {
    show();
    const toggle = screen.getByRole("switch");
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    expect(radio("24h").getAttribute("aria-checked")).toBe("true");

    fireEvent.click(toggle);
    await waitFor(() =>
      expect(api).toHaveBeenCalledWith("/api/account/preferences", {
        method: "PATCH",
        body: { mailVerificationHeuristics: "off" },
      }),
    );
    await waitFor(() => expect((radio("immediately") as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(radio("immediately"));
    await waitFor(() =>
      expect(api).toHaveBeenCalledWith("/api/account/preferences", {
        method: "PATCH",
        body: { mailVerificationCleanup: "immediately" },
      }),
    );
  });

  it("shows what was chosen before", () => {
    preferences = { mailVerificationHeuristics: "off", mailVerificationCleanup: "7d" };
    show();
    expect(screen.getByRole("switch").getAttribute("aria-checked")).toBe("false");
    expect(radio("7d").getAttribute("aria-checked")).toBe("true");
  });

  it("goes back to the stored value when saving fails", async () => {
    api.mockImplementation(() => Promise.reject(new Error("nope")));
    show();
    fireEvent.click(radio("off"));
    await waitFor(() => expect(api).toHaveBeenCalled());
    await waitFor(() => expect(radio("24h").getAttribute("aria-checked")).toBe("true"));
  });
});
