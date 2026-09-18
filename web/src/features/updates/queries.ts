import { useQuery } from "@tanstack/react-query";
import { api, type UpdatesView } from "@/lib/api";

export const updatesKey = ["admin", "updates"] as const;

/** What runs here and what is newer. Updating itself happens on the machine, with update.sh. */
export function useUpdates() {
  return useQuery({ queryKey: updatesKey, queryFn: () => api<UpdatesView>("/api/admin/updates") });
}
