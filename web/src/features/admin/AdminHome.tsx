import { useQuery } from "@tanstack/react-query";
import clsx from "clsx";
import { AtSign, Globe, HardDrive, Send, ShieldCheck, Users } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { Card, KeyValue } from "@/components/ui/Card";
import { LoadError, Loading } from "@/components/StatusViews";
import { useT } from "@/i18n";
import { api, type Overview } from "@/lib/api";
import { formatBytes, formatDuration } from "@/lib/format";
import { HealthCard } from "./HealthCard";
import { StatusTiles } from "./StatusTiles";

function Stat({
  icon: Icon,
  label,
  value,
  note,
  tone = "normal",
}: {
  icon: LucideIcon;
  label: string;
  value: ReactNode;
  note?: ReactNode;
  tone?: "normal" | "warning";
}) {
  return (
    <div className="rounded-card border border-hairline bg-surface p-4">
      <div className="flex items-center gap-2 text-[13px] font-semibold text-muted">
        <span
          className={clsx(
            "flex items-center justify-center rounded-full",
            "size-7",
            tone === "warning" ? "bg-warning-tint text-warning" : "bg-pink-tint text-pink-ink",
          )}
        >
          <Icon className="size-3.5" aria-hidden />
        </span>
        {label}
      </div>
      <p className="mt-2 text-xl font-bold tracking-[-0.02em]">{value}</p>
      {note && <p className="mt-1 text-[13px] text-muted">{note}</p>}
    </div>
  );
}

/** Server → Overview → Overview: the health lights, a tile per tab beside it, and the numbers. */
export function AdminHome() {
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

      <div className={clsx("grid gap-4", "grid-cols-2 md:grid-cols-3 xl:grid-cols-6")}>
        <Stat
          icon={Users}
          label={t("admin.cards.accounts")}
          value={counts.accounts}
          note={
            counts.disabledAccounts > 0 ? t("admin.accountsDisabled", { count: counts.disabledAccounts }) : undefined
          }
        />
        <Stat icon={Globe} label={t("admin.cards.domains")} value={counts.domains} />
        <Stat icon={AtSign} label={t("admin.cards.aliases")} value={counts.aliases} />
        <Stat
          icon={Send}
          label={t("admin.cards.queue")}
          value={counts.queuedMessages}
          tone={counts.deferredRecipients > 0 ? "warning" : "normal"}
        />
        <Stat icon={HardDrive} label={t("admin.cards.storage")} value={formatBytes(counts.usedBytes, language)} />
        <Stat icon={ShieldCheck} label={t("admin.cards.admins")} value={counts.admins} />
      </div>

      <Card title={t("admin.server.title")}>
        <KeyValue label={t("admin.server.hostname")} value={server.hostname} copy={server.hostname} />
        <KeyValue label={t("admin.server.version")} value={server.version} />
        <KeyValue label={t("admin.server.uptime")} value={formatDuration(server.uptimeSeconds, t)} />
      </Card>
    </div>
  );
}
