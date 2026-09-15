import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  api,
  type CloudflareResult,
  type DomainDetail,
  type DomainReport,
  type ServerCheck,
  type Session,
  type SetupStatus,
  type TestMailSent,
  type TestMailStatus,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

export function useSetupStatus() {
  return useQuery({ queryKey: ["setup"], queryFn: () => api<SetupStatus>("/api/setup") });
}

export function useVerifySetupCode() {
  return useMutation({
    mutationFn: (code: string) => api<{ ok: boolean }>("/api/setup/code", { method: "POST", body: { code } }),
  });
}

export interface FirstAdmin {
  code: string;
  domain: string;
  localPart: string;
  name: string;
  password: string;
}

export function useCompleteSetup() {
  return useMutation({
    mutationFn: (input: FirstAdmin) => api<Session>("/api/setup", { method: "POST", body: input }),
  });
}

const CHECK_KEY = ["admin", "setup", "check"];

/** The latest server check; `null` until one ran since the server started. */
export function useLastServerCheck() {
  return useQuery({ queryKey: CHECK_KEY, queryFn: () => api<ServerCheck | null>("/api/admin/setup/check") });
}

export function useRunServerCheck() {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: (blocklists: boolean) =>
      api<ServerCheck>("/api/admin/setup/check", { method: "POST", body: { blocklists } }),
    onSuccess: (check) => {
      queryClient.setQueryData(CHECK_KEY, check);
      void queryClient.invalidateQueries({ queryKey: ["admin", "health"] });
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

export function useSendTestMail() {
  const errorText = useErrorText();
  return useMutation({
    mutationFn: (external: string | null) =>
      api<TestMailSent>("/api/admin/setup/test-mail", { method: "POST", body: external ? { external } : {} }),
    onError: (error) => toast(errorText(error), "error"),
  });
}

/** Watches a sent test mail until it arrived and, for an outside address, until the reply is in. */
export function useTestMailStatus(sent: TestMailSent | null) {
  return useQuery({
    queryKey: ["admin", "setup", "test-mail", sent?.messageId],
    queryFn: () => api<TestMailStatus>(`/api/admin/setup/test-mail/${encodeURIComponent(sent!.messageId)}`),
    enabled: Boolean(sent),
    refetchInterval: (query) => {
      const status = query.state.data;
      const done = status?.arrived && (!sent?.external || status.replyFrom);
      return done ? false : sent?.external ? 5000 : 2000;
    },
  });
}

/** Puts the missing records into Cloudflare, then checks the domain again. */
export function useCloudflare(domain: string) {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: async (input: { token: string; replace: string[] }) => {
      const path = `/api/admin/domains/${encodeURIComponent(domain)}`;
      const { results } = await api<{ results: CloudflareResult[] }>(`${path}/dns/cloudflare`, {
        method: "POST",
        body: input,
      });
      const report = await api<DomainReport>(`${path}/check`, { method: "POST", body: {} }).catch(() => null);
      return { results, report };
    },
    onSuccess: ({ report }) => {
      if (report) {
        queryClient.setQueryData<DomainDetail>(
          ["admin", "domains", domain],
          (detail) => detail && { ...detail, report },
        );
      }
      void queryClient.invalidateQueries({ queryKey: ["admin", "domains"] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}
