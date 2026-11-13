import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "@/lib/api";

const api = vi.fn();
const navigate = vi.fn();
vi.mock("@/lib/api", async (actual) => ({
  ...(await actual<typeof import("@/lib/api")>()),
  api: (...args: unknown[]) => api(...args),
}));
vi.mock("@/lib/router", () => ({ navigate: (...args: unknown[]) => navigate(...args) }));

const { useLogout } = await import("./session");

function logout() {
  const client = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  return renderHook(() => useLogout(), { wrapper }).result;
}

afterEach(() => {
  api.mockReset();
  navigate.mockReset();
});

describe("useLogout", () => {
  it("keeps the user logged in when the server did not end the session", async () => {
    api.mockRejectedValue(new ApiError(0, "offline", "The server cannot be reached."));
    const result = logout();
    await act(() => result.current.mutateAsync().catch(() => {}));
    expect(navigate).not.toHaveBeenCalled();
  });

  it("goes to the login page when the session is gone, also when it had already ended", async () => {
    api.mockRejectedValue(new ApiError(401, "unauthorized", ""));
    const result = logout();
    await act(() => result.current.mutateAsync());
    expect(navigate).toHaveBeenCalledWith("/login", { replace: true });
  });
});
