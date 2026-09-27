import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  api,
  type DomainDetail,
  type DomainReport,
  type GroupInfo,
  type MtaStsMode,
  type ReportsView,
  type WhoMaySend,
} from "@/lib/api";
import { useT } from "@/i18n";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

const domainPath = (name: string) => `/api/admin/domains/${encodeURIComponent(name)}`;

export function useDomain(name: string) {
  return useQuery({ queryKey: ["admin", "domains", name], queryFn: () => api<DomainDetail>(domainPath(name)) });
}

function useDomainChanged() {
  const queryClient = useQueryClient();
  return (detail?: DomainDetail) => {
    if (detail) queryClient.setQueryData(["admin", "domains", detail.name], detail);
    void queryClient.invalidateQueries({ queryKey: ["admin", "domains"], exact: true });
    void queryClient.invalidateQueries({ queryKey: ["admin", "overview"] });
    void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
  };
}

/** A change to a domain with a toast for success or failure. */
function useDomainAction<Input>(run: (input: Input) => Promise<DomainDetail>, success: string) {
  const changed = useDomainChanged();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: run,
    onSuccess: (detail) => {
      changed(detail);
      toast(success, "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

export function useCreateDomain() {
  const changed = useDomainChanged();
  return useMutation({
    mutationFn: (name: string) => api<DomainDetail>("/api/admin/domains", { method: "POST", body: { name } }),
    onSuccess: (detail) => changed(detail),
  });
}

export function useRemoveDomain(name: string) {
  const changed = useDomainChanged();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: () => api<void>(domainPath(name), { method: "DELETE" }),
    onSuccess: () => changed(),
    onError: (error) => toast(errorText(error), "error"),
  });
}

export function useCheckDomain(name: string, success: string) {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: () => api<DomainReport>(`${domainPath(name)}/check`, { method: "POST", body: {} }),
    onSuccess: (report) => {
      queryClient.setQueryData<DomainDetail>(["admin", "domains", name], (detail) => detail && { ...detail, report });
      void queryClient.invalidateQueries({ queryKey: ["admin", "domains"], exact: true });
      void queryClient.invalidateQueries({ queryKey: ["admin", "overview"] });
      toast(success, "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

export function useSetCatchAll(name: string, success: string) {
  return useDomainAction(
    (login: string | null) => api<DomainDetail>(`${domainPath(name)}/catch-all`, { method: "PUT", body: { login } }),
    success,
  );
}

export function useSetForwardAddress(name: string) {
  const { t } = useT();
  const changed = useDomainChanged();
  return useMutation({
    mutationFn: (body: { local: string; targets: string[]; note: string }) =>
      api<DomainDetail>(`${domainPath(name)}/forwards`, { method: "PUT", body }),
    onSuccess: (detail, body) => {
      changed(detail);
      toast(t("domains.toasts.forwardSaved", { address: `${body.local}@${name}` }), "success");
    },
  });
}

export function useRemoveForwardAddress(name: string) {
  const { t } = useT();
  return useDomainAction(
    (local: string) =>
      api<DomainDetail>(`${domainPath(name)}/forwards/${encodeURIComponent(local)}`, { method: "DELETE" }),
    t("domains.toasts.forwardRemoved"),
  );
}

export function useSetMtaSts(name: string) {
  const changed = useDomainChanged();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: (mode: MtaStsMode | "off") =>
      api<DomainDetail>(`${domainPath(name)}/mta-sts`, { method: "PUT", body: { mode } }),
    onSuccess: (detail) => changed(detail),
    onError: (error) => toast(errorText(error), "error"),
  });
}

export function useDomainReports(name: string, days: number) {
  return useQuery({
    queryKey: ["admin", "domains", name, "reports", days],
    queryFn: () => api<ReportsView>(`${domainPath(name)}/reports?days=${days}`),
  });
}

export function useSetSelfService(name: string) {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: (on: boolean) => api<void>(`${domainPath(name)}/self-service`, { method: "PUT", body: { on } }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "domains", name] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

export function useRotateKeys(name: string, success: string) {
  return useDomainAction(
    () => api<DomainDetail>(`${domainPath(name)}/dkim/rotate`, { method: "POST", body: {} }),
    success,
  );
}

export function useActivateKeys(name: string, success: string) {
  return useDomainAction(
    (force: boolean) => api<DomainDetail>(`${domainPath(name)}/dkim/activate`, { method: "POST", body: { force } }),
    success,
  );
}

export function useRemoveKey(name: string, success: string) {
  return useDomainAction(
    (selector: string) =>
      api<DomainDetail>(`${domainPath(name)}/dkim/${encodeURIComponent(selector)}`, { method: "DELETE" }),
    success,
  );
}

export interface GroupBody {
  name?: string;
  whoMaySend?: WhoMaySend;
  membersMaySendAs?: boolean;
  members?: string[];
}

/** Creates, changes or removes a group of the domain, then loads the domain again. */
export function useGroupAction<Input>(name: string, run: (input: Input) => Promise<unknown>, success: string) {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: run,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "domains", name] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
      toast(success, "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

export function useCreateGroup(name: string, success: string) {
  return useGroupAction(
    name,
    (body: GroupBody & { local: string }) => api<GroupInfo>(`${domainPath(name)}/groups`, { method: "POST", body }),
    success,
  );
}

export function useUpdateGroup(name: string, success: string) {
  return useGroupAction(
    name,
    ({ local, ...body }: GroupBody & { local: string }) =>
      api<GroupInfo>(`${domainPath(name)}/groups/${encodeURIComponent(local)}`, { method: "PATCH", body }),
    success,
  );
}

export function useRemoveGroup(name: string, success: string) {
  return useGroupAction(
    name,
    (local: string) => api<void>(`${domainPath(name)}/groups/${encodeURIComponent(local)}`, { method: "DELETE" }),
    success,
  );
}

export function useSetMaskedAddresses(name: string) {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: (on: boolean) => api<void>(`${domainPath(name)}/masked-addresses`, { method: "PUT", body: { on } }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "domains", name] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}
