import { useQuery } from "@tanstack/react-query";
import { api, type ReportDetail, type ReportEntry, type ReportKind, type ReportsOverview } from "@/lib/api";

export function useReports(days: number) {
  return useQuery({
    queryKey: ["admin", "reports", days],
    queryFn: () => api<ReportsOverview>(`/api/admin/reports?days=${days}`),
  });
}

const reportPath = (domain: string, kind: ReportKind) =>
  `/api/admin/domains/${encodeURIComponent(domain)}/reports/${kind}`;

/** The reports themselves, newest first. Only fetched once someone opens the list. */
export function useReportList(domain: string, kind: ReportKind, enabled: boolean) {
  return useQuery({
    queryKey: ["admin", "reports", domain, kind, "list"],
    queryFn: () => api<{ reports: ReportEntry[] }>(`${reportPath(domain, kind)}?limit=25`),
    enabled,
  });
}

export function useReportDetail(domain: string, kind: ReportKind, id: number | null) {
  return useQuery({
    queryKey: ["admin", "reports", domain, kind, id],
    queryFn: () => api<ReportDetail>(`${reportPath(domain, kind)}/${id}`),
    enabled: id !== null,
  });
}
