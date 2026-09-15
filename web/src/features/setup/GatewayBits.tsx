import clsx from "clsx";
import { BookOpen, Globe, Link2, Unplug } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Dialog } from "@/components/ui/Dialog";
import { Field } from "@/components/ui/Field";
import { useT } from "@/i18n";
import type { Reachability } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";
import { Cancelled, usePasswordConfirmation } from "@/features/security/ConfirmPassword";
import { CheckLines, SubHeading } from "./SetupBits";
import { gatewayLines, reachabilityLines, recommendation } from "./reach";
import { useForgetGateway, useGateway, usePairGateway } from "./queries";

export const GATEWAY_DOCS = "https://github.com/MinifyX/UwUMail-Server/blob/main/docs/gateway.md";

const RECOMMENDATION_TONE = {
  gateway: "bg-warning-tint text-warning",
  direct: "bg-success-tint text-success",
  paired: "bg-success-tint text-success",
  unknown: "bg-canvas text-muted",
} as const;

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
