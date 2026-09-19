import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  api,
  type AppPasswordCreated,
  type AppPasswordInfo,
  type DomainSummary,
  type PasswordLinkCreated,
  type Person,
  type Protocols,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

const personPath = (login: string) => `/api/admin/people/${encodeURIComponent(login)}`;

export function usePeople() {
  return useQuery({ queryKey: ["admin", "people"], queryFn: () => api<Person[]>("/api/admin/people") });
}

export function usePerson(login: string) {
  return useQuery({ queryKey: ["admin", "people", login], queryFn: () => api<Person>(personPath(login)) });
}

export function useDomains() {
  return useQuery({ queryKey: ["admin", "domains"], queryFn: () => api<DomainSummary[]>("/api/admin/domains") });
}

/** Keeps list, detail and overview in step after a change to one person. */
function usePersonUpdated() {
  const queryClient = useQueryClient();
  return (person?: Person) => {
    // Change answers leave out the security summary of the detail view, so keep the one we have.
    if (person) queryClient.setQueryData<Person>(["admin", "people", person.login], (old) => ({ ...old, ...person }));
    void queryClient.invalidateQueries({ queryKey: ["admin", "people"], exact: true });
    void queryClient.invalidateQueries({ queryKey: ["admin", "overview"] });
    void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
  };
}

export interface NewPerson {
  address: string;
  name: string;
  admin: boolean;
  /** A mailbox for a program: no portal login, app passwords only. */
  service?: boolean;
  /** Only for a service: hand out an app password right away. */
  makePassword?: boolean;
  quotaBytes: number;
  password?: string;
}

/** What comes back from creating one: an invitation link for a person, a secret for a service. */
export interface PersonCreated {
  person: Person;
  link: PasswordLinkCreated | null;
  access: AppPasswordCreated | null;
}

export function useCreatePerson() {
  const updated = usePersonUpdated();
  return useMutation({
    mutationFn: (person: NewPerson) => api<PersonCreated>("/api/admin/people", { method: "POST", body: person }),
    onSuccess: (result) => updated(result.person),
  });
}

export interface PersonChanges {
  name?: string;
  admin?: boolean;
  /** Turns a person into a service, or a service back into a person. */
  service?: boolean;
  protocols?: Protocols;
  redirectTo?: string;
  quotaBytes?: number;
  disabled?: boolean;
}

/**
 * A change to one person, with a toast for success or failure. Every mutation of the
 * person page goes through here so that errors look the same everywhere.
 */
function usePersonAction<Input>(run: (input: Input) => Promise<Person | void>, success?: (input: Input) => string) {
  const updated = usePersonUpdated();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: run,
    onSuccess: (person, input) => {
      updated(person ?? undefined);
      if (success) toast(success(input), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

export function useUpdatePerson(login: string, success: (changes: PersonChanges) => string) {
  return usePersonAction(
    (changes: PersonChanges) => api<Person>(personPath(login), { method: "PATCH", body: changes }),
    success,
  );
}

export function useTrashPerson(login: string, success: () => string) {
  return usePersonAction(() => api<Person>(personPath(login), { method: "DELETE" }), success);
}

export function useRestorePerson(login: string, success: () => string) {
  return usePersonAction(() => api<Person>(`${personPath(login)}/restore`, { method: "POST", body: {} }), success);
}

export function usePurgePerson(login: string) {
  const updated = usePersonUpdated();
  return useMutation({
    mutationFn: (confirm: string) => api<void>(`${personPath(login)}/purge`, { method: "POST", body: { confirm } }),
    onSuccess: () => updated(),
  });
}

export function useAddAlias(login: string, success: (address: string) => string) {
  return usePersonAction(
    (address: string) => api<Person>(`${personPath(login)}/aliases`, { method: "POST", body: { address } }),
    success,
  );
}

export function useRemoveAlias(login: string, success: (address: string) => string) {
  return usePersonAction(
    (address: string) =>
      api<Person>(`${personPath(login)}/aliases/${encodeURIComponent(address)}`, { method: "DELETE" }),
    success,
  );
}

export function useCreatePasswordLink(login: string) {
  const updated = usePersonUpdated();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: () => api<PasswordLinkCreated>(`${personPath(login)}/password-link`, { method: "POST", body: {} }),
    onSuccess: () => updated(),
    onError: (error) => toast(errorText(error), "error"),
  });
}

export function useResetSecondFactors(login: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => api<void>(`${personPath(login)}/reset-second-factors`, { method: "POST", body: {} }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "people", login] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "health"] });
    },
  });
}

export function useSetExternalForwarding(login: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (blocked: boolean) =>
      api<void>(`${personPath(login)}/external-forwarding`, { method: "PUT", body: { blocked } }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "people", login] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
    },
  });
}

export function useSetAliasLimit(login: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (limit: number) => api<void>(`${personPath(login)}/alias-limit`, { method: "PUT", body: { limit } }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "people", login] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
    },
  });
}

export function useSetSendAsDomains(login: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (domains: string[]) =>
      api<{ domains: string[] }>(`${personPath(login)}/send-as-domains`, { method: "PUT", body: { domains } }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "people", login] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
    },
  });
}

/** A service cannot open its own security page, so an admin manages its app passwords here. */
export function useCreateServicePassword(login: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (name: string) =>
      api<AppPasswordCreated>(`${personPath(login)}/app-passwords`, { method: "POST", body: { name } }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "people", login] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
    },
  });
}

export function useRevokeServicePassword(login: string, success: (name: string) => string) {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: (password: AppPasswordInfo) =>
      api<void>(`${personPath(login)}/app-passwords/${password.id}`, { method: "DELETE" }),
    onSuccess: (_result, password) => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "people", login] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
      toast(success(password.name), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

export function useSetPassword(login: string) {
  const updated = usePersonUpdated();
  return useMutation({
    mutationFn: (password: string) => api<void>(`${personPath(login)}/password`, { method: "PUT", body: { password } }),
    onSuccess: () => updated(),
  });
}
