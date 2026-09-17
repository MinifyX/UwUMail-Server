import { useQuery } from "@tanstack/react-query";
import clsx from "clsx";
import { AtSign, Globe, HardDrive, Send, ShieldCheck, Users } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { NyuScene } from "@/components/nyu/scenes";
import { Card, KeyValue, PageHeader } from "@/components/ui/Card";
import { LoadError, Loading } from "@/components/StatusViews";
import { useT } from "@/i18n";
import { api, type Overview } from "@/lib/api";
import { formatBytes, formatDuration } from "@/lib/format";
import { usePrefs } from "@/state/prefs";
import { HealthCard, useHealth } from "./HealthCard";
import { UpdatesCard } from "./UpdatesCard";

function Stat({
  icon: Icon,
  label,
  value,
  note,
  tone = "normal",
  compact,
}: {
  icon: LucideIcon;
  label: string;
  value: ReactNode;
  note?: ReactNode;
  tone?: "normal" | "warning";
  compact: boolean;
}) {
  return (
    <div className={clsx("rounded-card border border-hairline bg-surface", compact ? "p-4" : "p-5")}>
      <div className="flex items-center gap-2 text-[13px] font-semibold text-muted">
        <span
          className={clsx(
            "flex items-center justify-center rounded-full",
            compact ? "size-7" : "size-9",
            tone === "warning" ? "bg-warning-tint text-warning" : "bg-pink-tint text-pink-ink",
          )}
        >
          <Icon className={compact ? "size-3.5" : "size-[18px]"} aria-hidden />
        </span>
        {label}
      </div>
      <p className={clsx("font-bold tracking-[-0.02em]", compact ? "mt-2 text-xl" : "mt-3 text-[28px]")}>{value}</p>
      {note && <p className="mt-1 text-[13px] text-muted">{note}</p>}
    </div>
  );
}

export function AdminHome() {
  const { t, i18n } = useT();
  const mode = usePrefs((s) => s.mode);
  const overview = useQuery({
    queryKey: ["admin", "overview"],
    queryFn: () => api<Overview>("/api/admin/overview"),
    refetchInterval: 30_000,
  });
  const health = useHealth();

  if (overview.isPending) return <Loading />;
  if (overview.isError) return <LoadError error={overview.error} onRetry={() => void overview.refetch()} />;
  const { counts, server } = overview.data;
  const compact = mode === "pro";
  const language = i18n.language;

  const queueNote =
    counts.deferredRecipients > 0
      ? t("admin.queueDeferred", { count: counts.deferredRecipients })
      : counts.queuedMessages > 0
        ? t("admin.queueWaiting", { count: counts.queuedMessages })
        : t("admin.queueEmpty");

  return (
    <div className="flex flex-col gap-5">
      <PageHeader
        title={t("admin.title")}
        intro={t("admin.intro")}
        art={!compact && health.data?.level === "ok" && <NyuScene name="done" className="h-auto w-[150px]" />}
      />

      <HealthCard />

      <div
        className={clsx(
          "grid gap-4",
          compact ? "grid-cols-2 md:grid-cols-3 xl:grid-cols-6" : "sm:grid-cols-2 lg:grid-cols-3",
        )}
      >
        <Stat
          compact={compact}
          icon={Users}
          label={t("admin.cards.accounts")}
          value={counts.accounts}
          note={
            counts.disabledAccounts > 0 ? t("admin.accountsDisabled", { count: counts.disabledAccounts }) : undefined
          }
        />
        <Stat compact={compact} icon={Globe} label={t("admin.cards.domains")} value={counts.domains} />
        <Stat compact={compact} icon={AtSign} label={t("admin.cards.aliases")} value={counts.aliases} />
        <Stat
          compact={compact}
          icon={Send}
          label={t("admin.cards.queue")}
          value={counts.queuedMessages}
          tone={counts.deferredRecipients > 0 ? "warning" : "normal"}
          note={compact ? undefined : queueNote}
        />
        <Stat
          compact={compact}
          icon={HardDrive}
          label={t("admin.cards.storage")}
          value={formatBytes(counts.usedBytes, language)}
        />
        {compact && <Stat compact icon={ShieldCheck} label={t("admin.cards.admins")} value={counts.admins} />}
      </div>

      <Card title={t("admin.server.title")}>
        <KeyValue label={t("admin.server.hostname")} value={server.hostname} copy={server.hostname} />
        <KeyValue label={t("admin.server.version")} value={server.version} />
        <KeyValue label={t("admin.server.uptime")} value={formatDuration(server.uptimeSeconds, t)} />
        {compact && <KeyValue label={t("admin.cards.queue")} value={queueNote} />}
      </Card>

      <UpdatesCard />
    </div>
  );
}
