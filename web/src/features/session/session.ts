import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, needsSecondFactor, setCsrfToken, type Info, type LoginResult, type Session } from "@/lib/api";
import { assertPasskey, type RequestOptionsJson } from "@/lib/webauthn";
import { navigate } from "@/lib/router";
import { usePrefs, type Prefs } from "@/state/prefs";

function adopt(session: Session): Session {
  setCsrfToken(session.csrfToken);
  usePrefs.getState().apply(session.preferences);
  return session;
}

async function loadSession(): Promise<Session | null> {
  const session = await api<Session | null>("/api/session");
  if (session) return adopt(session);
  setCsrfToken(null);
  return null;
}

/** The logged-in person, or `null` on the login page. */
export function useSession() {
  return useQuery({ queryKey: ["session"], queryFn: loadSession, staleTime: 60_000 });
}

export function useInfo() {
  return useQuery({ queryKey: ["info"], queryFn: () => api<Info>("/api/info"), staleTime: Infinity });
}

/** Takes over a fresh session and leaves the login page. */
export function useStartSession() {
  const queryClient = useQueryClient();
  return (session: Session, to = "/account") => {
    queryClient.setQueryData(["session"], adopt(session));
    const path = window.location.pathname;
    if (path === "/" || path === "/login" || path === "/setup" || path.startsWith("/password/")) {
      navigate(to, { replace: true });
    }
  };
}

/** The password step. Accounts with a second factor get a challenge instead of a session. */
export function useLogin() {
  const start = useStartSession();
  return useMutation({
    mutationFn: (credentials: { login: string; password: string }) =>
      api<LoginResult>("/api/auth/login", { method: "POST", body: credentials }),
    onSuccess: (result) => {
      if (!needsSecondFactor(result)) start(result);
    },
  });
}

/** The second step with a code from the authenticator app or a recovery code. */
export function useSecondFactorCode() {
  const start = useStartSession();
  return useMutation({
    mutationFn: (request: { token: string; code: string }) =>
      api<Session>("/api/auth/second-factor", { method: "POST", body: request }),
    onSuccess: (session) => start(session),
  });
}

/** The second step with a passkey: the browser asks for it, the server checks the signature. */
export function usePasskeyLogin() {
  const start = useStartSession();
  return useMutation({
    mutationFn: async (token: string) => {
      const options = await api<RequestOptionsJson>("/api/auth/passkey/options", { method: "POST", body: { token } });
      const credential = await assertPasskey(options);
      return api<Session>("/api/auth/passkey", { method: "POST", body: { token, credential } });
    },
    onSuccess: (session) => start(session),
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
