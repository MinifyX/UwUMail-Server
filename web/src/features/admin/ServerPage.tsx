import { ArrowLeftRight, ChartColumn, DatabaseBackup, LayoutDashboard, MailWarning } from "lucide-react";
import { TabbedPage } from "@/components/ui/TabbedPage";
import { BackupsPage } from "@/features/backups/BackupsPage";
import { MicrosoftPage } from "@/features/microsoft/MicrosoftPage";
import { MailFlowPage } from "@/features/setup/SetupPage";
import { StatsPage } from "@/features/stats/StatsPage";
import { useT } from "@/i18n";
import type { Session } from "@/lib/api";
import { AdminHome } from "./AdminHome";

/**
 * Server → Overview: how the server is doing, what happened, how mail comes and goes, whether
 * Microsoft takes it, and backups.
 */
export type ServerTab = "overview" | "stats" | "mailFlow" | "microsoft" | "backups";
export const SERVER_PATHS: Record<ServerTab, string> = {
  overview: "/admin",
  stats: "/admin/stats",
  mailFlow: "/admin/mail-flow",
  microsoft: "/admin/microsoft",
  backups: "/admin/backups",
};
const ICONS = {
  overview: LayoutDashboard,
  stats: ChartColumn,
  mailFlow: ArrowLeftRight,
  microsoft: MailWarning,
  backups: DatabaseBackup,
};
const INTROS: Record<ServerTab, string> = {
  overview: "admin.intro",
  stats: "stats.intro",
  mailFlow: "setup.page.intro",
  microsoft: "microsoft.intro",
  backups: "backups.intro",
};

export function ServerPage({ tab = "overview", session }: { tab?: ServerTab; session: Session }) {
  const { t } = useT();
  return (
    <TabbedPage<ServerTab>
      title={t("admin.title")}
      intro={t(INTROS[tab])}
      label={t("admin.tab")}
      value={tab}
      tabs={(Object.keys(SERVER_PATHS) as ServerTab[]).map((value) => ({
        value,
        to: SERVER_PATHS[value],
        icon: ICONS[value],
        label: t(`admin.tabs.${value}`),
      }))}
    >
      {tab === "overview" && <AdminHome />}
      {tab === "stats" && <StatsPage />}
      {tab === "mailFlow" && <MailFlowPage session={session} />}
      {tab === "microsoft" && <MicrosoftPage />}
      {tab === "backups" && <BackupsPage />}
    </TabbedPage>
  );
}
