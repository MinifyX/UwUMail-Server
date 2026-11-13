import { Button, ICONS, StatusDot, type StatusState } from "@uwusuite/design";
import { useQuery } from "@tanstack/react-query";
import { MailWarning } from "lucide-react";
import { useState } from "react";
import { TabbedPage } from "@/components/ui/TabbedPage";
import { BackupsPage } from "@/features/backups/BackupsPage";
import { MicrosoftPage } from "@/features/microsoft/MicrosoftPage";
import { MailFlowPage } from "@/features/setup/SetupPage";
import { StatsPage } from "@/features/stats/StatsPage";
import { useT } from "@/i18n";
import { CreatePersonDialog } from "@/features/people/CreatePersonDialog";
import { api, type HealthLevel, type Overview, type Session } from "@/lib/api";
import { AdminHome } from "./AdminHome";
import { useAlerts } from "./AlertsCard";
import { calmLevel } from "./calm";
import { useHealth } from "./HealthCard";

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
const KIND_ICONS = {
  overview: ICONS.overview,
  stats: ICONS.stats,
  mailFlow: ICONS.mailFlow,
  microsoft: MailWarning,
  backups: ICONS.backup,
};
const INTROS: Record<ServerTab, string> = {
  overview: "admin.intro",
  stats: "stats.intro",
  mailFlow: "setup.page.intro",
  microsoft: "microsoft.intro",
  backups: "backups.intro",
};

const DOTS: Record<HealthLevel, StatusState> = {
  ok: "online",
  unknown: "offline",
  warning: "error",
  problem: "danger",
};

/**
 * Under the big title: how the server is doing as a dot and in words, its name and version. The
 * same light as the simple view, from the same queries, so both views agree.
 */
function StatusLine() {
  const { t } = useT();
  const health = useHealth();
  const alerts = useAlerts();
  const overview = useQuery({
    queryKey: ["admin", "overview"],
    queryFn: () => api<Overview>("/api/admin/overview"),
    refetchInterval: 30_000,
  });
  const level = calmLevel(health.data, alerts.data);
  const server = overview.data?.server;
  return (
    <span className="flex flex-wrap items-center gap-x-2 gap-y-0.5" aria-live="polite">
      <StatusDot state={DOTS[level]} />
      <span className="font-semibold text-ink">{t(`adminView.headline.${level}`)}</span>
      {server && (
        <>
          <span aria-hidden>·</span>
          <span className="font-mono text-[13px]">{server.hostname}</span>
          <span aria-hidden>·</span>
          <span>{t("admin.versionShort", { version: server.version })}</span>
        </>
      )}
    </span>
  );
}

export function ServerPage({ tab = "overview", session }: { tab?: ServerTab; session: Session }) {
  const { t } = useT();
  const [creating, setCreating] = useState(false);
  const overview = tab === "overview";
  return (
    <TabbedPage<ServerTab>
      title={t("admin.title")}
      large={overview}
      intro={overview ? <StatusLine /> : t(INTROS[tab])}
      actions={
        overview && (
          <Button variant="primary" icon={ICONS.addContact} onClick={() => setCreating(true)}>
            {t("people.add")}
          </Button>
        )
      }
      label={t("admin.tab")}
      value={tab}
      tabs={(Object.keys(SERVER_PATHS) as ServerTab[]).map((value) => ({
        value,
        to: SERVER_PATHS[value],
        icon: KIND_ICONS[value],
        label: t(`admin.tabs.${value}`),
      }))}
    >
      {tab === "overview" && <AdminHome />}
      {tab === "stats" && <StatsPage />}
      {tab === "mailFlow" && <MailFlowPage session={session} />}
      {tab === "microsoft" && <MicrosoftPage />}
      {tab === "backups" && <BackupsPage />}
      {overview && <CreatePersonDialog open={creating} onClose={() => setCreating(false)} />}
    </TabbedPage>
  );
}
