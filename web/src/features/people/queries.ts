import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type DomainSummary, type PasswordLinkCreated, type Person } from "@/lib/api";
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
    if (person) queryClient.setQueryData(["admin", "people", person.login], person);
    void queryClient.invalidateQueries({ queryKey: ["admin", "people"], exact: true });
    void queryClient.invalidateQueries({ queryKey: ["admin", "overview"] });
    void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
  };
}

export interface NewPerson {
  address: string;
  name: string;
  admin: boolean;
  quotaBytes: number;
  password?: string;
}

export function useCreatePerson() {
  const updated = usePersonUpdated();
  return useMutation({
    mutationFn: (person: NewPerson) =>
      api<{ person: Person; link: PasswordLinkCreated | null }>("/api/admin/people", { method: "POST", body: person }),
    onSuccess: (result) => updated(result.person),
  });
}

export interface PersonChanges {
  name?: string;
  admin?: boolean;
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

export function useSetPassword(login: string) {
  const updated = usePersonUpdated();
  return useMutation({
    mutationFn: (password: string) => api<void>(`${personPath(login)}/password`, { method: "PUT", body: { password } }),
    onSuccess: () => updated(),
  });
}
