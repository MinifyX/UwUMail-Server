import clsx from "clsx";
import { BookOpen, Download, Globe, Link2, RefreshCw, RotateCcw, Server, Unplug } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Dialog } from "@/components/ui/Dialog";
import { Field } from "@/components/ui/Field";
import { useT } from "@/i18n";
import type { GatewayView, Reachability } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";
import { Cancelled, usePasswordConfirmation } from "@/features/security/ConfirmPassword";
import { CheckLines, SubHeading } from "./SetupBits";
import { gatewayLines, reachabilityLines, recommendation } from "./reach";
import { useForgetGateway, useGateway, useGatewayJob, usePairGateway, type GatewayVerb } from "./queries";

export const GATEWAY_DOCS = "https://github.com/MinifyX/UwUMail-Server/blob/main/docs/gateway.md";

const RECOMMENDATION_TONE = {
  gateway: "bg-warning-tint text-warning",
  direct: "bg-success-tint text-success",
  paired: "bg-success-tint text-success",
  unknown: "bg-canvas text-muted",
} as const;

/**
 * The buttons for the machine the gateway runs on, when a helper over there can carry them out.
 *
 * Without one this is absent and the check lines above keep showing the commands to copy, which is
 * what a gateway installed before this could do. The VPS belongs to the gateway alone, so unlike
 * the mail server's own machine there is no warning here about what else might be running.
 */
function GatewayMachineActions({ view }: { view: GatewayView }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const job = useGatewayJob();
  const { confirmed, dialog } = usePasswordConfirmation();

  const machine = view.machine;
  if (!view.canInstall || !machine) return null;
  const running = machine.job?.state === "running";
  const updates = machine.system?.updates ?? 0;

  const ask = (verb: GatewayVerb) => {
    void (async () => {
      try {
        await confirmed((password) => job.mutateAsync({ verb, password }));
      } catch (failure) {
        if (!(failure instanceof Cancelled)) toast(errorText(failure), "error");
      }
    })();
  };

  return (
    <div className="flex flex-col gap-3 border-t border-hairline pt-4">
      <SubHeading icon={Server}>{t("setup.gateway.machine.title")}</SubHeading>
      {machine.job && (
        <div className="rounded-control bg-canvas px-3 py-2">
          <p className="text-[13px] font-semibold">
            {t("setup.gateway.machine.states." + machine.job.state, { defaultValue: machine.job.state })}
            {machine.job.error && <span className="ml-2 font-normal text-danger">{machine.job.error}</span>}
          </p>
          {machine.job.log && (
            <pre className="mt-2 max-h-56 overflow-auto font-mono text-[12px] whitespace-pre-wrap">
              {machine.job.log}
            </pre>
          )}
        </div>
      )}
      <div className="flex flex-wrap gap-2">
        <Button size="sm" icon={RefreshCw} disabled={running || updates === 0} onClick={() => ask("os-update")}>
          {t("setup.gateway.machine.installUpdates")}
        </Button>
        {view.softwareVersion && (
          <Button size="sm" variant="primary" icon={Download} disabled={running} onClick={() => ask("gateway-update")}>
            {t("setup.gateway.machine.updateGateway", { version: view.softwareVersion })}
          </Button>
        )}
        <Button size="sm" variant="danger" icon={RotateCcw} disabled={running} onClick={() => ask("reboot")}>
          {t("setup.gateway.machine.restart")}
        </Button>
      </div>
      <p className="text-[12px] text-muted">
        {t("setup.gateway.machine.hint", {
          time: machine.checkedAt ? new Date(machine.checkedAt * 1000).toLocaleString(i18n.language) : "",
        })}
      </p>
      {dialog}
    </div>
  );
}

/** Where the server stands on the internet, and what Nyu makes of it. */
export function ReachabilityChecks({ reach, explain }: { reach: Reachability; explain: boolean }) {
  const { t } = useT();
  const advice = recommendation(reach);
  const sources = reach.addresses.flatMap((address) => (address.provider ? [address.provider] : []));
  return (
    <div className="flex flex-col gap-4">
      <p className={clsx("rounded-control px-3 py-2.5 text-[13px] font-semibold", RECOMMENDATION_TONE[advice])}>
        {t(`setup.reach.recommend.${advice}`)}
      </p>
      <div className="flex flex-col gap-3">
        <SubHeading icon={Globe}>{t("setup.reach.addressTitle")}</SubHeading>
        <CheckLines lines={reachabilityLines(reach)} explain={explain} />
      </div>
      {sources.length > 0 && (
        <p className="text-[12px] text-faint">
          {t("setup.reach.source")}{" "}
          {[...new Map(sources.map((source) => [source.key, source])).values()].map((source, index) => (
            <span key={source.key}>
              {index > 0 && ", "}
              <a href={source.source} target="_blank" rel="noreferrer" className="underline hover:text-ink">
                {t(`setup.reach.providers.${source.key}`)}
              </a>
            </span>
          ))}
        </p>
      )}
    </div>
  );
}

/** The gateway: its state, pairing with a code, and forgetting it. */
export function GatewayPanel({ hostname, explain }: { hostname: string; explain: boolean }) {
  const { t } = useT();
  const errorText = useErrorText();
  const gateway = useGateway();
  const pair = usePairGateway();
  const forget = useForgetGateway();
  const { confirmed, dialog } = usePasswordConfirmation();
  const [code, setCode] = useState("");
  const [error, setError] = useState<unknown>(null);
  const [another, setAnother] = useState(false);
  const [forgetOpen, setForgetOpen] = useState(false);

  if (gateway.isPending) return <Loading />;
  if (gateway.isError) return <LoadError error={gateway.error} onRetry={() => void gateway.refetch()} />;
  const view = gateway.data;
  const showForm = view.state === "none" || view.state === "refused" || another;

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    try {
      await confirmed((password) => pair.mutateAsync({ code: code.trim(), password }));
      setCode("");
      setAnother(false);
      toast(t("setup.gateway.paired"), "success");
    } catch (failure) {
      if (!(failure instanceof Cancelled)) setError(failure);
    }
  };

  const confirmForget = async () => {
    setForgetOpen(false);
    try {
      await confirmed((password) => forget.mutateAsync(password));
      toast(t("setup.gateway.forgotten"), "success");
    } catch (failure) {
      if (!(failure instanceof Cancelled)) toast(errorText(failure), "error");
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <CheckLines lines={gatewayLines(view, hostname)} explain={explain} />
      {view.fromConfig && view.state !== "none" && (
        <p className="rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">{t("setup.gateway.fromConfig")}</p>
      )}
      {/* Not behind `explain`: whoever pairs a gateway has to read this, in Pro mode as well. */}
      {showForm && (
        <p className="rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">{t("setup.gateway.vpsAlone")}</p>
      )}
      {showForm ? (
        <form className="flex flex-col gap-3" onSubmit={(event) => void submit(event)}>
          {explain && <p className="text-[13px] text-muted">{t("setup.gateway.howTo")}</p>}
          <Field
            label={t("setup.gateway.codeLabel")}
            hint={t("setup.gateway.codeHint")}
            error={error ? errorText(error) : undefined}
          >
            {(id) => (
              <textarea
                id={id}
                rows={3}
                required
                spellCheck={false}
                autoComplete="off"
                placeholder="uwugw1…"
                className="w-full rounded-control border border-line bg-surface px-3.5 py-2.5 font-mono text-[13px] break-all focus:border-pink focus:shadow-focus focus:outline-none"
                value={code}
                onChange={(event) => setCode(event.target.value)}
              />
            )}
          </Field>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <a
              href={GATEWAY_DOCS}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-1.5 text-[13px] font-semibold text-pink-ink hover:underline"
            >
              <BookOpen className="size-4" aria-hidden />
              {t("setup.reach.docs")}
            </a>
            <div className="flex gap-2">
              {another && <Button onClick={() => setAnother(false)}>{t("common.cancel")}</Button>}
              <Button type="submit" variant="primary" icon={Link2} busy={pair.isPending} disabled={!code.trim()}>
                {t("setup.gateway.pair")}
              </Button>
            </div>
          </div>
        </form>
      ) : (
        <div className="flex flex-wrap gap-2">
          <Button size="sm" icon={Link2} onClick={() => setAnother(true)}>
            {t("setup.gateway.pairOther")}
          </Button>
          <Button size="sm" variant="danger" icon={Unplug} busy={forget.isPending} onClick={() => setForgetOpen(true)}>
            {t("setup.gateway.forget")}
          </Button>
        </div>
      )}
      {!showForm && <GatewayMachineActions view={view} />}
      <Dialog open={forgetOpen} onClose={() => setForgetOpen(false)} title={t("setup.gateway.forgetTitle")} width="sm">
        <div className="flex flex-col gap-4 px-6 pt-1 pb-6">
          <p className="text-sm text-muted">{t("setup.gateway.forgetBody")}</p>
          <div className="flex justify-end gap-2">
            <Button onClick={() => setForgetOpen(false)}>{t("common.cancel")}</Button>
            <Button variant="danger" icon={Unplug} onClick={() => void confirmForget()}>
              {t("setup.gateway.forgetConfirm")}
            </Button>
          </div>
        </div>
      </Dialog>
      {dialog}
    </div>
  );
}
