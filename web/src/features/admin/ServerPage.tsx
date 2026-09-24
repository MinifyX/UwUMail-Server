import { ArrowLeftRight, DatabaseBackup, Download, LayoutDashboard } from "lucide-react";
import { TabbedPage } from "@/components/ui/TabbedPage";
import { BackupsPage } from "@/features/backups/BackupsPage";
import { MailFlowPage } from "@/features/setup/SetupPage";
import { UpdatesPage } from "@/features/updates/UpdatesPage";
import { useT } from "@/i18n";
import type { Session } from "@/lib/api";
import { AdminHome } from "./AdminHome";

/** Server → Overview: how the server is doing, how mail comes and goes, backups and updates. */
export type ServerTab = "overview" | "mailFlow" | "backups" | "updates";
export const SERVER_PATHS: Record<ServerTab, string> = {
  overview: "/admin",
  mailFlow: "/admin/mail-flow",
  backups: "/admin/backups",
  updates: "/admin/updates",
};
const ICONS = { overview: LayoutDashboard, mailFlow: ArrowLeftRight, backups: DatabaseBackup, updates: Download };
const INTROS: Record<ServerTab, string> = {
  overview: "admin.intro",
  mailFlow: "setup.page.intro",
  backups: "backups.intro",
  updates: "updates.intro",
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
      {tab === "mailFlow" && <MailFlowPage session={session} />}
      {tab === "backups" && <BackupsPage />}
      {tab === "updates" && <UpdatesPage />}
    </TabbedPage>
  );
}
