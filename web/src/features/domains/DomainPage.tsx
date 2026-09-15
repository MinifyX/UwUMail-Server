import { ArrowLeft, Globe, Info, KeyRound, RefreshCw, Trash2, X } from "lucide-react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Field, Select, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { ApiError, type DomainDetail } from "@/lib/api";
import { formatDate, formatDateTime } from "@/lib/format";
import { Link, navigate } from "@/lib/router";
import { usePrefs } from "@/state/prefs";
import { toast } from "@/state/toasts";
import { usePeople } from "@/features/people/queries";
import { CloudflarePanel } from "@/features/setup/SetupBits";
import { DnsStatusPill, RecordList } from "./DnsBits";
import { MtaStsCard, ReportsCard } from "./MtaStsCards";
import {
  useActivateKeys,
  useCheckDomain,
  useDomain,
  useRemoveDomain,
  useRemoveKey,
  useRotateKeys,
  useSetCatchAll,
  useSetSelfService,
} from "./queries";

function DnsCard({ domain }: { domain: DomainDetail }) {
  const { t, i18n } = useT();
  const pro = usePrefs((s) => s.mode) === "pro";
  const check = useCheckDomain(domain.name, t("domains.toasts.checked"));
  const report = domain.report;

  return (
    <Card
      title={t("domains.detail.dns")}
      action={
        <Button size="sm" icon={RefreshCw} busy={check.isPending} onClick={() => check.mutate()}>
          {check.isPending ? t("domains.detail.checking") : t("domains.detail.check")}
        </Button>
      }
    >
      <div className="flex flex-col gap-3">
        {!pro && <p className="text-[13px] text-muted">{t("domains.detail.dnsIntro")}</p>}
        {domain.setup.upstreamMx && (
          <p className="flex gap-2 rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
            <Info className="mt-0.5 size-4 shrink-0" aria-hidden />
            {t("domains.detail.upstream")}
          </p>
        )}
        {domain.setup.relayHost && (
          <p className="flex gap-2 rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
            <Info className="mt-0.5 size-4 shrink-0" aria-hidden />
            {t("domains.detail.relay", { relay: domain.setup.relayHost })}
          </p>
        )}
        {report ? (
          <>
            <p className="text-[12px] text-muted">
              {t("domains.detail.checkedAt", { time: formatDateTime(report.checkedAt, i18n.language) })}
              {" · "}
              {report.source === "authoritative"
                ? t("domains.detail.sourceAuthoritative", { servers: report.nameservers.join(", ") || "—" })
                : t("domains.detail.sourceResolver")}
            </p>
            <div className="mt-1">
              <RecordList records={report.records} domain={domain.name} explain={!pro} />
            </div>
            <CloudflarePanel domain={domain.name} report={report} explain={!pro} />
          </>
        ) : (
          <p className="rounded-control bg-pink-tint px-3 py-2.5 text-[13px] text-pink-ink">
            {t("domains.detail.neverChecked")}
          </p>
        )}
      </div>
    </Card>
  );
}

function CatchAllCard({ domain }: { domain: DomainDetail }) {
  const { t } = useT();
  const people = usePeople();
  const save = useSetCatchAll(domain.name, t("domains.toasts.catchAll"));
  const selfService = useSetSelfService(domain.name);
  const choices = (people.data ?? []).filter((person) => person.status !== "deleted");
  return (
    <Card title={t("domains.detail.catchAll")}>
      <Field label={t("domains.detail.catchAllTarget")} hint={t("domains.detail.catchAllHint")}>
        {(id) => (
          <Select
            id={id}
            value={domain.catchAll ?? ""}
            disabled={save.isPending}
            onChange={(event) => save.mutate(event.target.value || null)}
          >
            <option value="">{t("domains.detail.catchAllOff")}</option>
            {choices.map((person) => (
              <option key={person.login} value={person.login}>
                {person.name ? `${person.name} (${person.login})` : person.login}
              </option>
            ))}
          </Select>
        )}
      </Field>
      <div className="mt-4 border-t border-hairline pt-4">
        <Toggle
          checked={Boolean(domain.selfServiceAliases)}
          onChange={(on) => selfService.mutate(on)}
          label={t("domains.detail.selfService")}
          description={t("domains.detail.selfServiceHint")}
        />
      </div>
    </Card>
  );
}

function KeysCard({ domain }: { domain: DomainDetail }) {
  const { t, i18n } = useT();
  const pro = usePrefs((s) => s.mode) === "pro";
  const rotate = useRotateKeys(domain.name, t("domains.toasts.rotated"));
  const activate = useActivateKeys(domain.name, t("domains.toasts.activated"));
  const removeKey = useRemoveKey(domain.name, t("domains.toasts.keyRemoved"));
  const pending = domain.keys.some((key) => key.state === "pending");
  const retired = domain.keys.some((key) => key.state === "retired");
  const notPublished = activate.error instanceof ApiError && activate.error.code === "keysNotPublished";

  return (
    <Card title={t("domains.detail.keys")}>
      <div className="flex flex-col gap-3">
        {!pro && <p className="text-[13px] text-muted">{t("domains.detail.keysIntro")}</p>}
        <ul className="flex flex-col">
          {domain.keys.map((key) => (
            <li
              key={key.selector}
              className="flex min-h-11 items-center justify-between gap-3 border-b border-hairline py-1.5 last:border-b-0"
            >
              <span className="min-w-0">
                <span className="block truncate text-sm font-semibold">{key.selector}</span>
                <span className="block text-[12px] text-muted">
                  {key.algorithm === "rsa-sha256" ? "RSA" : "Ed25519"} · {t(`domains.detail.keyState.${key.state}`)} ·{" "}
                  {formatDate(key.createdAt, i18n.language)}
                </span>
              </span>
              {key.state === "retired" && (
                <IconButton
                  size="sm"
                  icon={X}
                  label={t("domains.detail.removeKey", { selector: key.selector })}
                  onClick={() => removeKey.mutate(key.selector)}
                />
              )}
            </li>
          ))}
        </ul>
        {pending ? (
          <div className="flex flex-col gap-2 rounded-control bg-pink-tint/60 p-3">
            <p className="text-[13px] text-pink-ink">{t("domains.detail.rotateSteps")}</p>
            <div className="flex flex-wrap gap-2">
              <Button
                variant="primary"
                icon={KeyRound}
                busy={activate.isPending}
                onClick={() => activate.mutate(false)}
              >
                {t("domains.detail.activate")}
              </Button>
              {notPublished && pro && (
                <Button variant="danger" onClick={() => activate.mutate(true)}>
                  {t("domains.detail.activateForce")}
                </Button>
              )}
            </div>
          </div>
        ) : (
          <Button
            className="self-start"
            icon={KeyRound}
            busy={rotate.isPending}
            onClick={() => rotate.mutate(undefined)}
          >
            {t("domains.detail.rotate")}
          </Button>
        )}
        {retired && <p className="text-[12px] text-muted">{t("domains.detail.retiredHint")}</p>}
      </div>
    </Card>
  );
}

export function DomainPage({ name }: { name: string }) {
  const { t } = useT();
  const query = useDomain(name);
  const remove = useRemoveDomain(name);

  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const domain = query.data;
  const unused = domain.people + domain.aliases === 0;

  return (
    <div className="flex flex-col gap-5">
      <Link
        to="/admin/domains"
        className="inline-flex items-center gap-1.5 self-start rounded-full text-[13px] font-semibold text-muted hover:text-ink"
      >
        <ArrowLeft className="size-4" aria-hidden />
        {t("domains.detail.back")}
      </Link>
      <header className="flex flex-wrap items-center gap-4">
        <span className="flex size-14 items-center justify-center rounded-full bg-pink-tint text-pink-ink">
          <Globe className="size-7" aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-[22px] font-bold tracking-[-0.01em]">{domain.name}</h1>
          <p className="text-sm text-muted">
            {t("domains.people", { count: domain.people })}
            {domain.aliases > 0 && ` · ${t("domains.aliases", { count: domain.aliases })}`}
          </p>
        </div>
        <DnsStatusPill status={domain.report?.status ?? null} />
      </header>

      <DnsCard domain={domain} />
      <ReportsCard domain={domain} />
      <div className="grid gap-5 md:grid-cols-2">
        <div className="flex flex-col gap-5">
          <MtaStsCard domain={domain} />
          <KeysCard domain={domain} />
        </div>
        <div className="flex flex-col gap-5">
          <CatchAllCard domain={domain} />
          <Card title={t("domains.detail.remove")}>
            <div className="flex flex-col items-start gap-3">
              <p className="text-[13px] text-muted">{t("domains.detail.removeHint")}</p>
              <Button
                variant="danger"
                icon={Trash2}
                disabled={!unused}
                busy={remove.isPending}
                onClick={() => {
                  if (!window.confirm(t("domains.detail.removeConfirm", { domain: domain.name }))) return;
                  remove.mutate(undefined, {
                    onSuccess: () => {
                      toast(t("domains.toasts.removed", { domain: domain.name }), "success");
                      navigate("/admin/domains", { replace: true });
                    },
                  });
                }}
              >
                {t("domains.detail.remove")}
              </Button>
            </div>
          </Card>
        </div>
      </div>
    </div>
  );
}
