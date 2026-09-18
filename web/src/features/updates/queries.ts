import { useQuery } from "@tanstack/react-query";
import { ApiError, api, type UpdateStatus, type UpdatesView } from "@/lib/api";

export const updatesKey = ["admin", "updates"] as const;

/** Whether something is going on right now. */
export const busy = (status?: UpdateStatus) => status?.state === "backup" || status?.state === "running";

/**
 * The update view, asked for often while an update runs.
 *
 * The long retry is the whole trick of this page: the update replaces the very server being asked,
 * so for a few seconds there is nobody to answer. Giving up after two tries would leave the page
 * showing an error for something that worked. It keeps knocking for about two minutes instead, and
 * the answer comes from the process that took the other one's place.
 */
export function useUpdates() {
  return useQuery({
    queryKey: updatesKey,
    queryFn: () => api<UpdatesView>("/api/admin/updates"),
    refetchInterval: (query) => (busy(query.state.data?.status) ? 2000 : false),
    retry: (count, error) => !(error instanceof ApiError && error.status < 500) && count < 60,
    retryDelay: 2000,
  });
}
