import { Download, RefreshCw, RotateCcw } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { Card, CopyButton } from "@/components/ui/Card";
import { useT } from "@/i18n";
import { formatDateTime } from "@/lib/format";
import { helperCan, helperOutdated, JobBox, jobBusy, updateCommand, useAskHost, useHost } from "./host";

/**
 * The machine the server runs on: what it has waiting, and the buttons that install it.
 *
 * Only there when a helper was installed beside the container (`deploy/host/`). Without one this
 * card is absent and the update commands stay something to copy, exactly as before.
 */
export function HostCard() {
  const { t, i18n } = useT();
  const query = useHost();
  const { ask, dialog } = useAskHost();

  if (query.isPending || query.isError || !query.data.available) return null;
  const { machine, job } = query.data;
  if (!machine) {
    return (
      <Card title={t("host.title")}>
        <p className="text-[13px] text-muted">{t("host.noReport")}</p>
      </Card>
    );
  }

  const unknownSystem = machine.kind !== "debian";
  // Never "yes" unless the helper said so. It answers null when it could not tell, and a warning
  // that sometimes stays quiet is worse than none at all.
  const alone = machine.alone === true;
  const running = jobBusy(job?.state);
  const outdated = helperOutdated(machine);
  const selfUpdate = helperCan(machine, "helper-update");
  const nothingToDo = machine.updates === 0 && !machine.rebootRequired;

  return (
    <Card title={t("host.title")}>
      <div className="flex flex-col gap-3">
        <p className="text-[13px] text-muted">
          {machine.name || t("host.unknownSystem")} ·{" "}
          {t("host.checked", { at: formatDateTime(machine.checkedAt, i18n.language) })}
        </p>

        {unknownSystem ? (
          <p className="rounded-control bg-canvas px-3 py-2 text-[13px]">{t("host.notApt")}</p>
        ) : (
          <p className="text-[13px]">
            {nothingToDo
              ? t("host.upToDate")
              : t("host.waiting", { count: machine.updates, security: machine.securityUpdates })}
          </p>
        )}

        {machine.rebootRequired && (
          <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
            {t("host.rebootNeeded")}
            {machine.rebootPackages.length > 0 && ` (${machine.rebootPackages.slice(0, 5).join(", ")})`}
          </p>
        )}

        {/* The one thing this card exists to say plainly. */}
        {!alone && (
          <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
            {machine.alone === false ? t("host.notAlone", { others: machine.others.join(", ") }) : t("host.cannotTell")}
          </p>
        )}
        <p className="text-[12px] text-muted">{t("host.liability")}</p>

        {outdated &&
          (selfUpdate ? (
            <p className="rounded-control bg-pink-tint px-3 py-2 text-[13px]">{t("host.helperNewer")}</p>
          ) : (
            <div className="flex flex-col gap-1.5 rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
              <p>{t("host.helperOld", { version: machine.helper ?? "1" })}</p>
              <div className="flex items-start gap-2 rounded-control bg-surface px-3 py-1.5 text-ink">
                <code className="min-w-0 flex-1 font-mono text-[12px] break-all">{updateCommand(machine)}</code>
                <CopyButton value={updateCommand(machine)} />
              </div>
            </div>
          ))}

        <JobBox view={query.data} />

        <div className="flex flex-wrap gap-2">
          <Button
            icon={RefreshCw}
            busy={ask.isPending}
            disabled={running || unknownSystem || machine.updates === 0}
            onClick={() => ask.mutate("os-update")}
          >
            {t("host.installUpdates")}
          </Button>
          {outdated && selfUpdate && (
            <Button icon={Download} disabled={running} onClick={() => ask.mutate("helper-update")}>
              {t("host.updateHelper")}
            </Button>
          )}
          <Button icon={RotateCcw} variant="danger" disabled={running} onClick={() => ask.mutate("reboot")}>
            {t("host.restart")}
          </Button>
        </div>
      </div>
      {dialog}
    </Card>
  );
}
