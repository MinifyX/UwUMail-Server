import clsx from "clsx";
import { ChevronRight, DatabaseBackup, Globe, RotateCw, UserPlus, Users } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Nyu } from "@/components/nyu/Nyu";
import { Button } from "@/components/ui/Button";
import { CreatePersonDialog } from "@/features/people/CreatePersonDialog";
import { useT } from "@/i18n";
import { api, type Health, type HealthLevel } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatRelative } from "@/lib/format";
import { Link } from "@/lib/router";
import { useBrand } from "@/state/brand";
import { toast } from "@/state/toasts";
import { useAlerts, useAlertText } from "./AlertsCard";
import { LEVELS, useFindingText, useHealth } from "./HealthCard";
import { calmItems, calmLevel } from "./calm";

/** Three lamps, the one for `level` lit. Not checked yet lights none of them fully. */
function TrafficLight({ level }: { level: HealthLevel }) {
  const { t } = useT();
  const lamps: { lit: boolean; color: string }[] = [
    { lit: level === "problem", color: "bg-danger" },
    { lit: level === "warning" || level === "unknown", color: "bg-warning" },
    { lit: level === "ok", color: "bg-success" },
  ];
  return (
    <div
      role="img"
      aria-label={t("adminView.light", { level: t(`health.level.${level}`) })}
      className="flex shrink-0 flex-col items-center gap-2 rounded-[28px] bg-ink/90 p-3 dark:bg-elevated"
    >
      {lamps.map((lamp, index) => (
        <span
          key={index}
          className={clsx(
            "size-12 rounded-full transition-opacity sm:size-14",
            lamp.color,
            lamp.lit ? (level === "unknown" ? "opacity-50" : "opacity-100 ring-4 ring-white/25") : "opacity-15",
          )}
        />
      ))}
    </div>
  );
}

function QuickAction({
  icon: Icon,
  label,
  to,
  onClick,
}: {
  icon: LucideIcon;
  label: string;
  to?: string;
  onClick?: () => void;
}) {
  const className =
    "flex items-center gap-3 rounded-card border border-hairline bg-surface p-4 text-sm font-semibold transition-colors hover:border-pink/40 hover:bg-pink-tint/30";
  const content = (
    <>
      <span className="flex size-8 items-center justify-center rounded-full bg-pink-tint text-pink-ink">
        <Icon className="size-4" aria-hidden />
      </span>
      <span className="flex-1 text-left">{label}</span>
      <ChevronRight className="size-4 text-faint" aria-hidden />
    </>
  );
  if (to) {
    return (
      <Link to={to} className={className}>
        {content}
      </Link>
    );
  }
  return (
    <button type="button" onClick={onClick} className={className}>
      {content}
    </button>
  );
}

/** The calm server overview: one traffic light, what to do about anything that is not green, and a few shortcuts. */
export function SimpleHome() {
  const { t, i18n } = useT();
  const mascot = useBrand((state) => state.mascot);
  const health = useHealth();
  const alerts = useAlerts();
  const findingText = useFindingText();
  const alertText = useAlertText();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [creating, setCreating] = useState(false);
  const check = useMutation({
    mutationFn: () => api<Health>("/api/admin/health/check", { method: "POST" }),
    onSuccess: (data) => queryClient.setQueryData(["admin", "health"], data),
    onError: (error) => toast(errorText(error), "error"),
  });

  const items = calmItems(health.data, alerts.data);
  const level = calmLevel(health.data, alerts.data);
  const needing = items.filter((item) => item.level === level).length;

  return (
    <div className="flex flex-col gap-5">
      <section
        className="flex flex-col items-center gap-5 rounded-card border border-hairline bg-surface p-6 sm:flex-row sm:items-center"
        aria-labelledby="calm-title"
        aria-live="polite"
      >
        <TrafficLight level={level} />
        <div className="min-w-0 flex-1 text-center sm:text-left">
          <h2 id="calm-title" className="text-2xl font-bold tracking-[-0.02em]">
            {t(`adminView.headline.${level}`)}
          </h2>
          <p className="mt-1 text-sm text-muted">
            {level === "warning" || level === "problem"
              ? t(`adminView.sentence.${level}`, { count: needing })
              : t(`adminView.sentence.${level}`)}
          </p>
          <p className="mt-2 text-[12px] text-faint">
            {health.data?.checkedAt
              ? t("health.checkedAt", { time: formatRelative(health.data.checkedAt, i18n.language) })
              : t("health.notChecked")}
          </p>
          <Button className="mt-3" size="sm" icon={RotateCw} busy={check.isPending} onClick={() => check.mutate()}>
            {check.isPending ? t("health.checking") : t("health.checkNow")}
          </Button>
        </div>
        {mascot && (
          <svg viewBox="60 100 392 330" className="nyu-host nyu-blink hidden h-auto w-36 shrink-0 md:block" aria-hidden>
            <Nyu mood={LEVELS[level].mood} />
          </svg>
        )}
      </section>

      {items.length > 0 && (
        <section className="rounded-card border border-hairline bg-surface p-5" aria-labelledby="calm-todo">
          <h2 id="calm-todo" className="mb-2 text-[15px] font-bold">
            {t("adminView.todo")}
          </h2>
          <ul>
            {items.map((item) => {
              const { text, hint } =
                item.source === "health"
                  ? {
                      text: findingText(item.finding),
                      hint: t(`health.hints.${item.finding.code}`, { defaultValue: "" }),
                    }
                  : alertText(item.alert);
              return (
                <li key={item.key} className="flex items-start gap-3 border-b border-hairline py-3 last:border-b-0">
                  <span className={clsx("mt-1.5 size-2.5 shrink-0 rounded-full", LEVELS[item.level].dot)} aria-hidden />
                  <div className="min-w-0 flex-1">
                    <p className="text-sm font-semibold">
                      <span className="sr-only">{t(`health.level.${item.level}`)}: </span>
                      {text}
                    </p>
                    {hint && <p className="mt-0.5 text-[13px] text-muted">{hint}</p>}
                    {item.link && (
                      <Link
                        to={item.link}
                        className="mt-1 inline-flex items-center gap-0.5 text-[13px] font-semibold text-pink-ink hover:underline"
                      >
                        {t("health.open")}
                        <ChevronRight className="size-3.5" aria-hidden />
                      </Link>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        </section>
      )}

      <section aria-labelledby="calm-quick">
        <h2 id="calm-quick" className="mb-2 text-[15px] font-bold">
          {t("adminView.quick.title")}
        </h2>
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          <QuickAction icon={Users} label={t("adminView.quick.people")} to="/admin/people" />
          <QuickAction icon={UserPlus} label={t("adminView.quick.addPerson")} onClick={() => setCreating(true)} />
          <QuickAction icon={Globe} label={t("adminView.quick.domains")} to="/admin/domains" />
          <QuickAction icon={DatabaseBackup} label={t("adminView.quick.backups")} to="/admin/backups" />
        </div>
      </section>
      <CreatePersonDialog open={creating} onClose={() => setCreating(false)} />
    </div>
  );
}
