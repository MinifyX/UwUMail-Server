import { useQuery } from "@tanstack/react-query";
import clsx from "clsx";
import { ArrowLeftRight, ChevronRight, DatabaseBackup, Download, History } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { describe } from "@/features/log/LogPage";
import { useUpdates } from "@/features/updates/queries";
import { useT } from "@/i18n";
import { api, type AuditRecord, type BackupsView, type HealthLevel, type Overview } from "@/lib/api";
import { formatRelative } from "@/lib/format";
import { Link } from "@/lib/router";
import { helperOutdated, useHost } from "./host";
import { LEVELS, useHealth } from "./HealthCard";

/** A light like the health overview's, plus pink for "something new, nothing wrong". */
type Tone = HealthLevel | "news";
const DOTS: Record<Tone, string> = {
  ok: LEVELS.ok.dot,
  unknown: LEVELS.unknown.dot,
  warning: LEVELS.warning.dot,
  problem: LEVELS.problem.dot,
  news: "bg-pink",
};

const WORST: HealthLevel[] = ["problem", "warning", "unknown", "ok"];

/** A tile that says one thing about one part of the server and opens the tab that has the rest. */
function Tile({
  to,
  icon: Icon,
  title,
  tone,
  text,
  note,
}: {
  to: string;
  icon: LucideIcon;
  title: string;
  tone: Tone;
  text: string;
  note?: string | null;
}) {
  const { t } = useT();
  return (
    <Link
      to={to}
      className="group flex flex-col gap-1.5 rounded-card border border-hairline bg-surface p-4 transition-colors hover:border-pink/40 hover:bg-pink-tint/30"
    >
      <span className="flex items-center gap-2 text-[13px] font-semibold text-muted">
        <span className="flex size-7 items-center justify-center rounded-full bg-pink-tint text-pink-ink">
          <Icon className="size-3.5" aria-hidden />
        </span>
        <span className="flex-1">{title}</span>
        <ChevronRight className="size-4 text-faint transition-transform group-hover:translate-x-0.5" aria-hidden />
      </span>
      <span className="flex items-start gap-2 text-sm font-semibold">
        <span className={clsx("mt-1.5 size-2.5 shrink-0 rounded-full", DOTS[tone])} aria-hidden />
        <span>
          {text}
          <span className="sr-only"> ({tone === "news" ? t("admin.tiles.news") : t(`health.level.${tone}`)})</span>
        </span>
      </span>
      {note && <span className="pl-[18px] text-[13px] text-muted">{note}</span>}
    </Link>
  );
}

function MailFlowTile({ counts }: { counts: Overview["counts"] }) {
  const { t } = useT();
  const health = useHealth();
  const areas = (health.data?.areas ?? []).filter((area) => area.area === "delivery" || area.area === "gateway");
  const level: HealthLevel = health.data
    ? (WORST.find((candidate) => areas.some((area) => area.level === candidate)) ?? "unknown")
    : "unknown";
  const note =
    counts.deferredRecipients > 0
      ? t("admin.queueDeferred", { count: counts.deferredRecipients })
      : counts.queuedMessages > 0
        ? t("admin.queueWaiting", { count: counts.queuedMessages })
        : t("admin.queueEmpty");
  return (
    <Tile
      to="/admin/mail-flow"
      icon={ArrowLeftRight}
      title={t("admin.tabs.mailFlow")}
      tone={level}
      text={t(`admin.tiles.mailFlow.${level}`)}
      note={note}
    />
  );
}

function BackupsTile() {
  const { t, i18n } = useT();
  const query = useQuery({ queryKey: ["admin", "backups"], queryFn: () => api<BackupsView>("/api/admin/backups") });
  const title = t("admin.tabs.backups");
  if (!query.data) {
    return (
      <Tile to="/admin/backups" icon={DatabaseBackup} title={title} tone="unknown" text={t("admin.tiles.loading")} />
    );
  }
  const { status, target, enabled, running } = query.data;
  const last = status.lastSuccessAt;
  const failed = Boolean(status.lastError) && (status.lastAttemptAt ?? 0) >= (last ?? 0);
  const lastText = last ? t("admin.tiles.backups.last", { time: formatRelative(last, i18n.language) }) : null;
  // A nightly backup that has not worked for two days is worth a look even without an error.
  const stale = last !== null && query.dataUpdatedAt / 1000 - last > 2 * 86_400;

  let tone: Tone = "ok";
  let text = lastText ?? t("admin.tiles.backups.never");
  let note: string | null = null;
  if (!target) {
    tone = "warning";
    text = t("admin.tiles.backups.none");
  } else if (running) {
    text = t("admin.tiles.backups.running");
    note = lastText;
  } else if (failed) {
    tone = "problem";
    text = t("admin.tiles.backups.failed");
    note = lastText;
  } else if (!enabled) {
    tone = "warning";
    text = t("admin.tiles.backups.paused");
    note = lastText;
  } else if (!last || stale) {
    tone = "warning";
  }
  return <Tile to="/admin/backups" icon={DatabaseBackup} title={title} tone={tone} text={text} note={note} />;
}

function UpdatesTile() {
  const { t } = useT();
  const query = useUpdates();
  const host = useHost();
  const title = t("admin.tabs.updates");
  if (!query.data) {
    // GitHub being away is no reason for a red tile; the tab says what went wrong.
    const text = query.isError ? t("admin.tiles.updates.unknown") : t("admin.tiles.loading");
    return <Tile to="/admin/updates" icon={Download} title={title} tone="unknown" text={text} />;
  }
  const { build, info } = query.data;
  const edge = !build.release;
  const newest = info.releases[0];
  const available = edge ? (info.behind ?? 0) > 0 : Boolean(newest);
  const machine = host.data?.available ? host.data.machine : null;
  const note = machine?.rebootRequired
    ? t("admin.tiles.updates.reboot")
    : helperOutdated(machine)
      ? t("admin.tiles.updates.helper")
      : machine && machine.updates > 0
        ? t("admin.tiles.updates.system", { count: machine.updates })
        : null;
  return (
    <Tile
      to="/admin/updates"
      icon={Download}
      title={title}
      tone={available ? "news" : machine?.rebootRequired ? "warning" : "ok"}
      text={
        available
          ? edge
            ? t("admin.tiles.updates.edge", { count: info.behind ?? 0 })
            : t("admin.tiles.updates.available", { version: newest?.version })
          : t("admin.tiles.updates.current", { version: build.version })
      }
      note={note}
    />
  );
}

function ChangesTile() {
  const { t, i18n } = useT();
  const query = useQuery({
    queryKey: ["admin", "audit", "latest"],
    queryFn: () => api<AuditRecord[]>("/api/admin/audit?limit=1"),
  });
  const latest = query.data?.[0];
  return (
    <Tile
      to="/admin/logs/changes"
      icon={History}
      title={t("admin.tiles.changes.title")}
      tone={latest ? "ok" : "unknown"}
      text={latest ? describe(latest, t) : query.data ? t("admin.tiles.changes.none") : t("admin.tiles.loading")}
      note={latest ? formatRelative(latest.at, i18n.language) : null}
    />
  );
}

/** One tile each for the tabs beside the overview, and for what changed last. */
export function StatusTiles({ counts }: { counts: Overview["counts"] }) {
  return (
    <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
      <MailFlowTile counts={counts} />
      <BackupsTile />
      <UpdatesTile />
      <ChangesTile />
    </div>
  );
}
