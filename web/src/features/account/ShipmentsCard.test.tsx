import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.fn();
let preferences: Record<string, unknown> = {};
vi.mock("@/lib/api", () => ({ api: (...args: unknown[]) => api(...args) }));
vi.mock("@/features/session/session", () => ({ useSession: () => ({ data: { preferences } }) }));
vi.mock("@/lib/errors", () => ({ useErrorText: () => (error: unknown) => String(error) }));
vi.mock("@/i18n", () => ({ useT: () => ({ t: (key: string) => key, i18n: { language: "en" } }) }));

const { ShipmentsCard } = await import("./ShipmentsCard");

function show() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <ShipmentsCard />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  api.mockReset();
  api.mockImplementation(() => Promise.resolve({}));
  preferences = {};
});
afterEach(cleanup);

describe("ShipmentsCard", () => {
  it("is on unless switched off, and saves the change as a preference", async () => {
    show();
    const toggle = screen.getByRole("switch");
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    fireEvent.click(toggle);
    await waitFor(() =>
      expect(api).toHaveBeenCalledWith("/api/account/preferences", { method: "PATCH", body: { shipments: "off" } }),
    );
  });

  it("shows when it was switched off", () => {
    preferences = { shipments: "off" };
    show();
    expect(screen.getByRole("switch").getAttribute("aria-checked")).toBe("false");
  });
});
