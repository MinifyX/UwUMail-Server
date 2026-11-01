import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.fn();
let preferences: Record<string, unknown> = {};
vi.mock("@/lib/api", () => ({ api: (...args: unknown[]) => api(...args) }));
vi.mock("@/features/session/session", () => ({ useSession: () => ({ data: { preferences } }) }));
vi.mock("@/i18n", () => ({ useT: () => ({ t: (key: string) => key, i18n: { language: "en" } }) }));

const { VerificationCard } = await import("./VerificationCard");

function show() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <VerificationCard />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  api.mockReset();
  api.mockImplementation(() => Promise.resolve({}));
  preferences = {};
});
afterEach(cleanup);

describe("verification codes", () => {
  it("starts with the server's defaults and saves each change as a preference", async () => {
    show();
    const toggle = screen.getByRole("switch");
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    const select = screen.getByLabelText("mailbox.verification.cleanup") as HTMLSelectElement;
    expect(select.value).toBe("24h");

    fireEvent.click(toggle);
    await waitFor(() =>
      expect(api).toHaveBeenCalledWith("/api/account/preferences", {
        method: "PATCH",
        body: { mailVerificationHeuristics: "off" },
      }),
    );
    await waitFor(() => expect(select.disabled).toBe(false));
    fireEvent.change(select, { target: { value: "immediately" } });
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
    expect((screen.getByLabelText("mailbox.verification.cleanup") as HTMLSelectElement).value).toBe("7d");
  });
});
