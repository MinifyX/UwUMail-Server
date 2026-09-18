import { useQuery } from "@tanstack/react-query";
import { api, type ReportsOverview } from "@/lib/api";

export function useReports(days: number) {
  return useQuery({
    queryKey: ["admin", "reports", days],
    queryFn: () => api<ReportsOverview>(`/api/admin/reports?days=${days}`),
  });
}
