import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  api,
  type AccountMaskedPolicy,
  type AppPasswordCreated,
  type AppPasswordInfo,
  type DomainSummary,
  type OAuthGrantInfo,
  type PasswordLinkCreated,
  type Person,
  type PersonMaskedPolicy,
  type Protocols,
  type SharedMailboxMember,
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

/** The domains people, aliases and groups can go on: every one but those only for masked addresses. */
export function useMailDomains() {
  return useQuery({
    queryKey: ["admin", "domains"],
    queryFn: () => api<DomainSummary[]>("/api/admin/domains"),
    select: (domains) => domains.filter((domain) => domain.kind !== "masked"),
  });
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
  /** Whether this person may open their mailbox in the browser. */
  webmail?: boolean;
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

/** Where one account may make masked addresses; null parts go by its domain. */
export function useSetPersonMaskedPolicy(login: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (policy: AccountMaskedPolicy) =>
      api<PersonMaskedPolicy>(`${personPath(login)}/masked-policy`, { method: "PUT", body: policy }),
    onSuccess: (maskedPolicy) => {
      queryClient.setQueryData<Person>(["admin", "people", login], (old) => old && { ...old, maskedPolicy });
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

export interface NewSharedMailbox {
  address: string;
  name: string;
  quotaBytes: number;
  members: { login: string; maySend: boolean }[];
}

export function useCreateSharedMailbox() {
  const updated = usePersonUpdated();
  return useMutation({
    mutationFn: (shared: NewSharedMailbox) =>
      api<{ person: Person; members: SharedMailboxMember[] }>("/api/admin/shared-mailboxes", {
        method: "POST",
        body: shared,
      }),
    // The detail view starts from this answer, members included.
    onSuccess: (result) => updated({ ...result.person, members: result.members }),
  });
}

export function useSetSharedMembers(login: string, success: string) {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: (members: { login: string; maySend: boolean }[]) =>
      api<SharedMailboxMember[]>(`/api/admin/shared-mailboxes/${encodeURIComponent(login)}/members`, {
        method: "PUT",
        body: { members },
      }),
    onSuccess: (members) => {
      queryClient.setQueryData<Person>(["admin", "people", login], (old) => old && { ...old, members });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
      toast(success, "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

/** Turns a person or a service into a shared mailbox with these members. */
export function useMakeSharedMailbox(login: string, success: string) {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: (members: { login: string; maySend: boolean }[]) =>
      api<{ person: Person; members: SharedMailboxMember[] }>(`${personPath(login)}/shared-mailbox`, {
        method: "POST",
        body: { members },
      }),
    onSuccess: () => {
      // Its app passwords and members come with the detail view, so fetch that again.
      void queryClient.invalidateQueries({ queryKey: ["admin", "people"] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "overview"] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
      toast(success, "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

/** Turns a shared mailbox back into a plain service. */
export function useEndSharedMailbox(login: string, success: () => string) {
  return usePersonAction(() => api<Person>(`${personPath(login)}/shared-mailbox`, { method: "DELETE" }), success);
}

/** Signs one of a person's OAuth apps out, e.g. for a lost phone. */
export function useRevokePersonGrant(login: string, success: (name: string) => string) {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: (grant: OAuthGrantInfo) =>
      api<void>(`${personPath(login)}/oauth-grants/${grant.id}`, { method: "DELETE" }),
    onSuccess: (_result, grant) => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "people", login] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
      toast(success(grant.clientName), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

/** Where the person's password is checked: here or at the LDAP directory. */
export function useSetAuthSource(login: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (source: "local" | "ldap") =>
      api<void>(`${personPath(login)}/auth-source`, { method: "PUT", body: { source } }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "people"] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
    },
  });
}
