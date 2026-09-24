import { ChartNoAxesColumn, Globe, Users } from "lucide-react";
import { TabbedPage } from "@/components/ui/TabbedPage";
import { DomainsPage } from "@/features/domains/DomainsPage";
import { ReportsPage } from "@/features/reports/ReportsPage";
import { useT } from "@/i18n";
import type { Session } from "@/lib/api";
import { PeoplePage } from "./PeoplePage";

/** Server → Accounts & domains: who has a mailbox here, for which domains, and what others report. */
export type DirectoryTab = "people" | "domains" | "reports";
export const DIRECTORY_PATHS: Record<DirectoryTab, string> = {
  people: "/admin/people",
  domains: "/admin/domains",
  reports: "/admin/reports",
};
const ICONS = { people: Users, domains: Globe, reports: ChartNoAxesColumn };

export function DirectoryPage({ tab = "people", session }: { tab?: DirectoryTab; session: Session }) {
  const { t } = useT();
  return (
    <TabbedPage<DirectoryTab>
      title={t("directory.title")}
      intro={t(`${tab}.intro`)}
      label={t("directory.tab")}
      value={tab}
      tabs={(Object.keys(DIRECTORY_PATHS) as DirectoryTab[]).map((value) => ({
        value,
        to: DIRECTORY_PATHS[value],
        icon: ICONS[value],
        label: t(`${value}.title`),
      }))}
    >
      {tab === "people" && <PeoplePage session={session} />}
      {tab === "domains" && <DomainsPage />}
      {tab === "reports" && <ReportsPage />}
    </TabbedPage>
  );
}
