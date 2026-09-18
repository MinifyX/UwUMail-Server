import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { RefreshCw, RotateCcw } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Cancelled, usePasswordConfirmation } from "@/features/security/ConfirmPassword";
import { useT } from "@/i18n";
import { api, type HostView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { toast } from "@/state/toasts";

type Verb = "os-update" | "reboot";

/** While a job runs, ask often enough that its output moves. */
const BUSY_MS = 2000;

/**
 * The machine the server runs on: what it has waiting, and the buttons that install it.
 *
 * Only there when a helper was installed beside the container (`deploy/host/`). Without one this
 * card is absent and the update commands stay something to copy, exactly as before.
 */
export function HostCard() {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const { confirmed, dialog } = usePasswordConfirmation();

  const query = useQuery({
    queryKey: ["admin", "host"],
    queryFn: () => api<HostView>("/api/admin/host"),
    refetchInterval: (query) => (query.state.data?.job?.state === "running" ? BUSY_MS : false),
  });

  const ask = useMutation({
    mutationFn: (verb: Verb) =>
      confirmed((password) => api<HostView>("/api/admin/host/jobs", { method: "POST", body: { verb, password } })),
    onSuccess: (view) => {
      queryClient.setQueryData(["admin", "host"], view);
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
    },
    onError: (error) => {
      if (!(error instanceof Cancelled)) toast(errorText(error), "error");
    },
  });

  if (query.isPending || query.isError || !query.data.available) return null;
  const { machine, job, log } = query.data;
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
  const running = job?.state === "running";
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

        {job && (
          <div className="rounded-control bg-canvas px-3 py-2">
            <p className="text-[13px] font-semibold">
              {t(`host.states.${job.state}`, { defaultValue: job.state })}
              {job.error && <span className="ml-2 font-normal text-danger">{job.error}</span>}
            </p>
            {log && <pre className="mt-2 max-h-64 overflow-auto font-mono text-[12px] whitespace-pre-wrap">{log}</pre>}
          </div>
        )}

        <div className="flex flex-wrap gap-2">
          <Button
            icon={RefreshCw}
            busy={ask.isPending}
            disabled={running || unknownSystem || machine.updates === 0}
            onClick={() => ask.mutate("os-update")}
          >
            {t("host.installUpdates")}
          </Button>
          <Button icon={RotateCcw} variant="danger" disabled={running} onClick={() => ask.mutate("reboot")}>
            {t("host.restart")}
          </Button>
        </div>
      </div>
      {dialog}
    </Card>
  );
}
