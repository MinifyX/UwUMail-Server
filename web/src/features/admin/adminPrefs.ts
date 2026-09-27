import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useSession } from "@/features/session/session";
import { api, type Session } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

/** The calm view with just the traffic light, or everything. Existing admins keep everything. */
export type AdminView = "simple" | "full";
/** Which alert mails an admin gets. */
export type AlertMails = "all" | "problems" | "none";

/** The menu entries the calm view keeps in sight; the others fold away under "More". */
const CALM_NAV = ["/admin", "/admin/people"];

export function inCalmNav(to: string): boolean {
  return CALM_NAV.includes(to);
}

export function adminViewOf(preferences: Record<string, unknown> | undefined): AdminView {
  return preferences?.adminView === "simple" ? "simple" : "full";
}

export function alertMailsOf(preferences: Record<string, unknown> | undefined): AlertMails {
  const value = preferences?.adminAlerts;
  return value === "problems" || value === "none" ? value : "all";
}

export function useAdminPrefs() {
  const session = useSession();
  const preferences = session.data?.preferences;
  return { view: adminViewOf(preferences), alertMails: alertMailsOf(preferences) };
}

/** Stores an admin preference and shows it at once; a failure puts the old value back. */
export function useSaveAdminPref() {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: (changes: { adminView?: AdminView; adminAlerts?: AlertMails }) =>
      api<Record<string, unknown>>("/api/account/preferences", { method: "PATCH", body: changes }),
    onMutate: (changes) => {
      const previous = queryClient.getQueryData<Session | null>(["session"]);
      if (previous) {
        queryClient.setQueryData<Session>(["session"], {
          ...previous,
          preferences: { ...previous.preferences, ...changes },
        });
      }
      return previous;
    },
    onSuccess: (preferences) => {
      const current = queryClient.getQueryData<Session | null>(["session"]);
      if (current) queryClient.setQueryData<Session>(["session"], { ...current, preferences });
    },
    onError: (error, _changes, previous) => {
      if (previous) queryClient.setQueryData(["session"], previous);
      toast(errorText(error), "error");
    },
  });
}
