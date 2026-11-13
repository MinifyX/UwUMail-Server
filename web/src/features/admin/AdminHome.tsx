import { Segmented } from "@uwusuite/design";
import { useQuery } from "@tanstack/react-query";
import { AtSign, Globe, HardDrive, Send, ShieldCheck, Users } from "lucide-react";
import { Card, KeyValue } from "@/components/ui/Card";
import { StatTile } from "@/components/ui/StatTile";
import { LoadError, Loading } from "@/components/StatusViews";
import { MicrosoftBanner } from "@/features/microsoft/MicrosoftBanner";
import { useT } from "@/i18n";
import { api, type Overview } from "@/lib/api";
import { formatBytes, formatDuration } from "@/lib/format";
import { useAdminPrefs, useSaveAdminPref, type AdminView } from "./adminPrefs";
import { AlertMailChoice, AlertsCard } from "./AlertsCard";
import { HealthCard } from "./HealthCard";
import { SimpleHome } from "./SimpleHome";
import { StatusTiles } from "./StatusTiles";

/** Simple or everything, for this admin; at the top of the overview so it is easy to find again. */
function ViewSwitch() {
  const { t } = useT();
  const { view } = useAdminPrefs();
  const save = useSaveAdminPref();
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
      <Segmented<AdminView>
        label={t("adminView.label")}
        value={view}
        onChange={(value) => save.mutate({ adminView: value })}
        options={(["simple", "full"] as const).map((value) => ({ value, label: t(`adminView.${value}`) }))}
      />
      <span className="min-w-0 flex-1 basis-60 text-[12px] text-muted">{t(`adminView.hint.${view}`)}</span>
    </div>
  );
}

/** Server → Overview → Overview: simple (one light and what to do) or everything, as the admin chose. */
export function AdminHome() {
  const { view } = useAdminPrefs();
  const { t } = useT();
  return (
    <div className="flex flex-col gap-5">
      <ViewSwitch />
      <MicrosoftBanner />
      {view === "simple" ? (
        <>
          <SimpleHome />
          <Card title={t("alerts.mail.title")}>
            <AlertMailChoice />
          </Card>
        </>
      ) : (
        <FullHome />
      )}
    </div>
  );
}

/** Everything: the health lights, a tile per tab beside it, the alerts and the numbers. */
function FullHome() {
  const { t, i18n } = useT();
  const overview = useQuery({
    queryKey: ["admin", "overview"],
    queryFn: () => api<Overview>("/api/admin/overview"),
    refetchInterval: 30_000,
  });

  if (overview.isPending) return <Loading />;
  if (overview.isError) return <LoadError error={overview.error} onRetry={() => void overview.refetch()} />;
  const { counts, server } = overview.data;
  const language = i18n.language;

  return (
    <div className="flex flex-col gap-5">
      <HealthCard />
      <StatusTiles counts={counts} />
      <AlertsCard />

      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-6">
        <StatTile
          icon={Users}
          label={t("admin.cards.accounts")}
          value={counts.accounts}
          note={
            counts.disabledAccounts > 0 ? t("admin.accountsDisabled", { count: counts.disabledAccounts }) : undefined
          }
        />
        <StatTile icon={Globe} label={t("admin.cards.domains")} value={counts.domains} />
        <StatTile icon={AtSign} label={t("admin.cards.aliases")} value={counts.aliases} />
        <StatTile
          icon={Send}
          label={t("admin.cards.queue")}
          value={counts.queuedMessages}
          tone={counts.deferredRecipients > 0 ? "warning" : "normal"}
        />
        <StatTile icon={HardDrive} label={t("admin.cards.storage")} value={formatBytes(counts.usedBytes, language)} />
        <StatTile icon={ShieldCheck} label={t("admin.cards.admins")} value={counts.admins} />
      </div>

      <Card title={t("admin.server.title")}>
        <KeyValue label={t("admin.server.hostname")} value={server.hostname} copy={server.hostname} />
        <KeyValue label={t("admin.server.version")} value={server.version} />
        <KeyValue label={t("admin.server.uptime")} value={formatDuration(server.uptimeSeconds, t)} />
      </Card>
    </div>
  );
}
