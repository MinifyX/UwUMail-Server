import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.fn();
let preferences: Record<string, unknown> = {};
let features: Record<string, boolean> = { shipments: true };
vi.mock("@/lib/api", () => ({ api: (...args: unknown[]) => api(...args) }));
vi.mock("@/features/session/session", () => ({ useSession: () => ({ data: { preferences, features } }) }));
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
  features = { shipments: true };
});
afterEach(cleanup);

describe("ShipmentsCard", () => {
  it("is on unless switched off, and switches off only once that is confirmed", async () => {
    show();
    const toggle = screen.getByRole("switch");
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    fireEvent.click(toggle);
    fireEvent.click(screen.getByRole("button", { name: "common.cancel", hidden: true }));
    expect(api).not.toHaveBeenCalled();
    expect(toggle.getAttribute("aria-checked")).toBe("true");

    fireEvent.click(toggle);
    fireEvent.click(screen.getByRole("button", { name: "account.shipments.confirmOff", hidden: true }));
    await waitFor(() =>
      expect(api).toHaveBeenCalledWith("/api/account/preferences", { method: "PATCH", body: { shipments: "off" } }),
    );
  });

  it("switches on without asking", async () => {
    preferences = { shipments: "off" };
    show();
    fireEvent.click(screen.getByRole("switch"));
    await waitFor(() =>
      expect(api).toHaveBeenCalledWith("/api/account/preferences", { method: "PATCH", body: { shipments: "on" } }),
    );
  });

  it("shows when it was switched off", () => {
    preferences = { shipments: "off" };
    show();
    expect(screen.getByRole("switch").getAttribute("aria-checked")).toBe("false");
  });

  it("is gone when the admin switched parcels off for the server", () => {
    features = { shipments: false };
    show();
    expect(screen.queryByRole("switch")).toBeNull();
  });

  it("offers the carrier lookup only when the admin turned it on, and switches it off without asking", async () => {
    features = { shipments: true, shipmentLookup: true };
    show();
    const lookup = screen.getAllByRole("switch")[1]!;
    expect(lookup.getAttribute("aria-checked")).toBe("true");
    fireEvent.click(lookup);
    await waitFor(() =>
      expect(api).toHaveBeenCalledWith("/api/account/preferences", {
        method: "PATCH",
        body: { shipmentLookup: "off" },
      }),
    );
  });
});
