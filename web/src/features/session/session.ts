import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, ApiError, setCsrfToken, type Info, type Session } from "@/lib/api";
import { navigate } from "@/lib/router";
import { usePrefs, type Prefs } from "@/state/prefs";

function adopt(session: Session): Session {
  setCsrfToken(session.csrfToken);
  usePrefs.getState().apply(session.preferences);
  return session;
}

async function loadSession(): Promise<Session | null> {
  try {
    return adopt(await api<Session>("/api/session"));
  } catch (error) {
    if (error instanceof ApiError && error.status === 401) {
      setCsrfToken(null);
      return null;
    }
    throw error;
  }
}

/** The logged-in person, or `null` on the login page. */
export function useSession() {
  return useQuery({ queryKey: ["session"], queryFn: loadSession, staleTime: 60_000 });
}

export function useInfo() {
  return useQuery({ queryKey: ["info"], queryFn: () => api<Info>("/api/info"), staleTime: Infinity });
}

export function useLogin() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (credentials: { login: string; password: string }) =>
      api<Session>("/api/auth/login", { method: "POST", body: credentials }),
    onSuccess: (session) => {
      queryClient.setQueryData(["session"], adopt(session));
      const path = window.location.pathname;
      if (path === "/" || path === "/login" || path === "/setup") navigate("/account", { replace: true });
    },
  });
}

export function useLogout() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => api<void>("/api/auth/logout", { method: "POST", body: {} }),
    onSettled: () => {
      setCsrfToken(null);
      queryClient.clear();
      queryClient.setQueryData(["session"], null);
      navigate("/login", { replace: true });
    },
  });
}

/** Changes preferences right away and stores them on the server; a failure rolls them back. */
export function useSavePrefs() {
  return useMutation({
    mutationFn: (changes: Partial<Prefs>) =>
      api<Record<string, unknown>>("/api/account/preferences", { method: "PATCH", body: changes }),
    onMutate: (changes) => {
      const state = usePrefs.getState();
      const previous = Object.fromEntries(Object.keys(changes).map((key) => [key, state[key as keyof Prefs]]));
      state.apply(changes);
      return previous as Partial<Prefs>;
    },
    onError: (_error, _changes, previous) => {
      if (previous) usePrefs.getState().apply(previous);
    },
  });
}
