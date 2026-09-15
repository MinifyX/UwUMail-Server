import "@fontsource-variable/manrope";
import "./styles/app.css";
import "./i18n";
import { MutationCache, QueryCache, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./app/App";
import { ApiError, setCsrfToken } from "./lib/api";

// `pnpm dev --mode mock` runs the app against sample data; production builds drop this.
if (import.meta.env.MODE === "mock") await import("./dev/mockApi");

/** A session that ran out (or was ended elsewhere) sends the app back to the login page. */
function onError(error: unknown) {
  if (error instanceof ApiError && error.code === "notLoggedIn") {
    setCsrfToken(null);
    queryClient.setQueryData(["session"], null);
  }
}

const queryClient: QueryClient = new QueryClient({
  queryCache: new QueryCache({ onError }),
  mutationCache: new MutationCache({ onError }),
  defaultOptions: {
    queries: {
      staleTime: 15_000,
      refetchOnWindowFocus: true,
      // Login and permission answers do not get better by asking again.
      retry: (count, error) => !(error instanceof ApiError && error.status < 500) && count < 2,
    },
  },
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </StrictMode>,
);
