import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Pause, Play, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { EmptyState } from "@/components/ui/EmptyState";
import { Field, Select, Toggle } from "@/components/ui/Field";
import { useDomains, usePeople } from "@/features/people/queries";
import { useT } from "@/i18n";
import { api } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatNumber } from "@/lib/format";
import { toast } from "@/state/toasts";
import {
  ADMIN_ASSIST,
  FEATURES,
  byDayAndPerson,
  byPerson,
  formatDay,
  usageSum,
  type AdminAssistView,
  type AdminProvider,
  type AdminUsageView,
  type AssistPolicy,
  type UsageSum,
} from "./model";
import { Notice, QuotaText, Tag } from "./parts";
import { ProviderDialog } from "./ProviderDialog";

const adminKey = ["admin", "assist"] as const;
const adminUsageKey = ["admin", "assist", "usage"] as const;

function PolicyCard({ policy }: { policy: AssistPolicy }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [draft, setDraft] = useState<AssistPolicy | null>(null);
  const shown = draft ?? policy;
  const save = useMutation({
    mutationFn: (body: AssistPolicy) => api<AssistPolicy>(`${ADMIN_ASSIST}/policy`, { method: "PUT", body }),
    onSuccess: (next) => {
      queryClient.setQueryData<AdminAssistView>(adminKey, (old) => (old ? { ...old, policy: next } : old));
      setDraft(null);
      toast(t("common.saved"), "success");
    },
    onError: (failure) => toast(errorText(failure), "error"),
  });

  return (
    <Card title={t("assist.admin.policyTitle")}>
      <p className="-mt-1 mb-4 text-[13px] text-muted">{t("assist.admin.policyIntro")}</p>
      <div className="flex flex-col gap-4">
        {FEATURES.map((feature) => (
          <Toggle
            key={feature}
            checked={shown.features[feature]}
            onChange={(value) => setDraft({ ...shown, features: { ...shown.features, [feature]: value } })}
            label={t(`assist.features.${feature}`)}
            description={t(`assist.featureHints.${feature}`)}
          />
        ))}
        <div className="flex flex-col gap-4 border-t border-hairline pt-4">
          <Toggle
            checked={shown.allowPersonal}
            onChange={(value) =>
              setDraft({ ...shown, allowPersonal: value, allowPersonalPrivate: value && shown.allowPersonalPrivate })
            }
            label={t("assist.admin.allowPersonal")}
            description={t("assist.admin.allowPersonalHint")}
          />
          <Toggle
            checked={shown.allowPersonalPrivate}
            disabled={!shown.allowPersonal}
            onChange={(value) => setDraft({ ...shown, allowPersonalPrivate: value })}
            label={t("assist.admin.allowPrivate")}
            description={t("assist.admin.allowPrivateHint")}
          />
          {shown.allowPersonal && shown.allowPersonalPrivate && (
            <Notice tone="danger" title={t("assist.admin.ssrfTitle")}>
              {t("assist.admin.ssrfWarning")}
            </Notice>
          )}
        </div>
      </div>
      <div className="mt-4 flex items-center justify-end gap-2 border-t border-hairline pt-4">
        {draft && (
          <Button variant="ghost" onClick={() => setDraft(null)}>
            {t("assist.choices.reset")}
          </Button>
        )}
        <Button variant="primary" disabled={!draft} busy={save.isPending} onClick={() => draft && save.mutate(draft)}>
          {t("common.save")}
        </Button>
      </div>
    </Card>
  );
}

/** Who may use a provider, in a few words. */
function accessText(provider: AdminProvider, t: ReturnType<typeof useT>["t"]): string {
  if (provider.access === "domains") {
    return t("assist.admin.accessDomains", { list: provider.domains.join(", ") });
  }
  if (provider.access === "people") {
    return t("assist.admin.accessPeople", { count: provider.people.length });
  }
  return t("assist.access.everyone");
}

function ProvidersCard({ view }: { view: AdminAssistView }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const domains = useDomains();
  const people = usePeople();
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<AdminProvider | null>(null);
  const refresh = () => void queryClient.invalidateQueries({ queryKey: adminKey });
  const setEnabled = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) =>
      api<AdminProvider>(`${ADMIN_ASSIST}/providers/${id}`, { method: "PATCH", body: { enabled } }),
    onSuccess: refresh,
    onError: (failure) => toast(errorText(failure), "error"),
  });
  const remove = useMutation({
    mutationFn: (id: number) => api<void>(`${ADMIN_ASSIST}/providers/${id}`, { method: "DELETE" }),
    onSuccess: () => {
      refresh();
      toast(t("assist.provider.removed"), "success");
    },
    onError: (failure) => toast(errorText(failure), "error"),
  });
  const kinds = view.kinds.filter((kind) => !kind.personalOnly);
  const dialogProps = {
    mode: "admin" as const,
    kinds,
    basePath: `${ADMIN_ASSIST}/providers`,
    queryKey: adminKey,
    domainSuggestions: (domains.data ?? []).map((domain) => domain.name),
    peopleSuggestions: (people.data ?? [])
      .filter((person) => person.status !== "deleted" && person.role !== "service")
      .map((person) => person.login),
  };

  return (
    <Card title={t("assist.admin.providersTitle")}>
      <p className="-mt-1 mb-3 text-[13px] text-muted">{t("assist.admin.providersIntro")}</p>
      <div className="flex flex-col gap-4">
        {view.providers.length === 0 ? (
          <EmptyState
            compact
            scene="addons"
            title={t("assist.admin.providersEmptyTitle")}
            body={t("assist.admin.providersEmpty")}
          />
        ) : (
          <ul className="flex flex-col">
            {view.providers.map((provider) => {
              const kind = view.kinds.find((candidate) => candidate.kind === provider.kind);
              return (
                <li
                  key={provider.id}
                  className="flex flex-wrap items-center gap-3 border-b border-hairline py-3 last:border-b-0"
                >
                  <span className="min-w-0 flex-1">
                    <span className="flex flex-wrap items-center gap-1.5">
                      <span className="truncate text-sm font-semibold">{provider.name}</span>
                      {!provider.enabled && <Tag tone="muted">{t("assist.admin.off")}</Tag>}
                    </span>
                    <span className="block text-[12px] text-muted">
                      {[
                        kind?.name ?? provider.kind,
                        provider.model ? t("assist.provider.model", { model: provider.model }) : null,
                        provider.fastModel && provider.fastModel !== provider.model
                          ? t("assist.provider.fastModel", { model: provider.fastModel })
                          : null,
                      ]
                        .filter(Boolean)
                        .join(" · ")}
                    </span>
                    {provider.baseUrl && (
                      <span className="block truncate font-mono text-[12px] text-muted">{provider.baseUrl}</span>
                    )}
                    <span className="block text-[12px] text-muted">
                      {accessText(provider, t)} ·{" "}
                      {provider.features.length === FEATURES.length
                        ? t("assist.admin.allFeatures")
                        : provider.features.map((feature) => t(`assist.features.${feature}`)).join(", ")}
                    </span>
                    <span className="block text-[12px] text-muted">
                      <QuotaText
                        quota={{ requestsPerDay: provider.requestsPerDay, tokensPerDay: provider.tokensPerDay }}
                      />
                      {provider.hasKey &&
                        provider.keyHint &&
                        ` · ${t("assist.provider.key", { hint: provider.keyHint })}`}
                    </span>
                  </span>
                  <div className="flex items-center gap-1.5">
                    <IconButton
                      icon={provider.enabled ? Pause : Play}
                      label={provider.enabled ? t("assist.admin.disable") : t("assist.admin.enable")}
                      onClick={() => setEnabled.mutate({ id: provider.id, enabled: !provider.enabled })}
                    />
                    <Button size="sm" variant="ghost" onClick={() => setEditing(provider)}>
                      {t("assist.provider.edit")}
                    </Button>
                    <IconButton
                      icon={Trash2}
                      label={t("assist.provider.remove")}
                      onClick={() => {
                        if (window.confirm(t("assist.admin.removeConfirm", { name: provider.name }))) {
                          remove.mutate(provider.id);
                        }
                      }}
                    />
                  </div>
                </li>
              );
            })}
          </ul>
        )}
        <div className="flex justify-end">
          <Button variant="primary" icon={Plus} onClick={() => setAdding(true)}>
            {t("assist.admin.addProvider")}
          </Button>
        </div>
      </div>
      <ProviderDialog open={adding} onClose={() => setAdding(false)} {...dialogProps} />
      <ProviderDialog
        open={editing !== null}
        onClose={() => setEditing(null)}
        provider={editing ?? undefined}
        {...dialogProps}
      />
    </Card>
  );
}

function SumCells({ sum, max }: { sum: UsageSum; max?: number }) {
  const { i18n } = useT();
  const number = (value: number) => formatNumber(value, i18n.language);
  return (
    <>
      <td className="px-2 py-1.5 text-right tabular-nums">
        <span className="flex items-center justify-end gap-2">
          {max !== undefined && max > 0 && (
            <span className="hidden h-1.5 w-16 overflow-hidden rounded-full bg-canvas sm:block" aria-hidden>
              <span
                className="block h-full rounded-full bg-pink"
                style={{ width: `${Math.max(2, (sum.requests / max) * 100)}%` }}
              />
            </span>
          )}
          {number(sum.requests)}
        </span>
      </td>
      <td className="px-2 py-1.5 text-right tabular-nums">{number(sum.inputTokens)}</td>
      <td className="py-1.5 pl-2 text-right tabular-nums">{number(sum.outputTokens)}</td>
    </>
  );
}

function SumHeads() {
  const { t } = useT();
  return (
    <>
      <th scope="col" className="px-2 py-1.5 text-right font-semibold">
        {t("assist.usage.requests")}
      </th>
      <th scope="col" className="px-2 py-1.5 text-right font-semibold whitespace-nowrap">
        {t("assist.usage.inputTokens")}
      </th>
      <th scope="col" className="py-1.5 pl-2 text-right font-semibold whitespace-nowrap">
        {t("assist.usage.outputTokens")}
      </th>
    </>
  );
}

/** Requests and tokens per day and person over the last 30 days. */
function UsageCard() {
  const { t, i18n } = useT();
  const [person, setPerson] = useState("");
  const usage = useQuery({
    queryKey: adminUsageKey,
    queryFn: () => api<AdminUsageView>(`${ADMIN_ASSIST}/usage?days=30`),
    refetchInterval: 60_000,
  });
  if (usage.isPending) return <Loading />;
  if (usage.isError) return <LoadError error={usage.error} onRetry={() => void usage.refetch()} />;
  const all = usage.data.days;
  const rows = person ? all.filter((row) => row.login === person) : all;
  const people = byPerson(all);
  const days = byDayAndPerson(rows);
  const sum = usageSum(rows);
  const maxPerson = Math.max(0, ...people.map((entry) => entry.requests));
  const maxDay = Math.max(0, ...days.map((entry) => entry.requests));

  return (
    <Card title={t("assist.admin.usageTitle")}>
      <p className="-mt-1 mb-3 text-[13px] text-muted">{t("assist.admin.usageIntro")}</p>
      {all.length === 0 ? (
        <p className="text-[13px] text-muted">{t("assist.usage.empty")}</p>
      ) : (
        <div className="flex flex-col gap-5">
          <dl className="grid grid-cols-3 gap-3">
            {(
              [
                ["requests", sum.requests],
                ["inputTokens", sum.inputTokens],
                ["outputTokens", sum.outputTokens],
              ] as const
            ).map(([key, value]) => (
              <div key={key} className="rounded-control bg-canvas px-3 py-2">
                <dt className="text-[12px] text-muted">{t(`assist.usage.${key}`)}</dt>
                <dd className="text-lg font-bold tabular-nums">{formatNumber(value, i18n.language)}</dd>
              </div>
            ))}
          </dl>

          <div className="flex flex-col gap-2">
            <p className="text-[13px] font-semibold text-muted">{t("assist.admin.perPerson")}</p>
            <div className="overflow-x-auto">
              <table className="w-full min-w-[480px] text-[13px]">
                <caption className="sr-only">{t("assist.admin.perPerson")}</caption>
                <thead>
                  <tr className="border-b border-hairline text-left text-muted">
                    <th scope="col" className="py-1.5 pr-3 font-semibold">
                      {t("assist.usage.person")}
                    </th>
                    <SumHeads />
                  </tr>
                </thead>
                <tbody>
                  {people.map((entry) => (
                    <tr key={entry.login} className="border-b border-hairline last:border-b-0">
                      <th scope="row" className="py-1.5 pr-3 text-left font-semibold break-all">
                        {entry.login}
                      </th>
                      <SumCells sum={entry} max={maxPerson} />
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>

          <div className="flex flex-col gap-2">
            <div className="flex flex-wrap items-end justify-between gap-3">
              <p className="text-[13px] font-semibold text-muted">{t("assist.admin.perDay")}</p>
              <Field label={t("assist.usage.person")} className="w-full sm:w-64">
                {(id) => (
                  <Select id={id} value={person} onChange={(event) => setPerson(event.target.value)}>
                    <option value="">{t("assist.admin.allPeople")}</option>
                    {people.map((entry) => (
                      <option key={entry.login} value={entry.login}>
                        {entry.login}
                      </option>
                    ))}
                  </Select>
                )}
              </Field>
            </div>
            <div className="overflow-x-auto">
              <table className="w-full min-w-[560px] text-[13px]">
                <caption className="sr-only">{t("assist.admin.perDay")}</caption>
                <thead>
                  <tr className="border-b border-hairline text-left text-muted">
                    <th scope="col" className="py-1.5 pr-3 font-semibold">
                      {t("assist.usage.day")}
                    </th>
                    <th scope="col" className="px-2 py-1.5 font-semibold">
                      {t("assist.usage.person")}
                    </th>
                    <SumHeads />
                  </tr>
                </thead>
                <tbody>
                  {days.map((entry) => (
                    <tr key={`${entry.day}\n${entry.login}`} className="border-b border-hairline">
                      <th scope="row" className="py-1.5 pr-3 text-left font-semibold whitespace-nowrap">
                        {formatDay(entry.day, i18n.language)}
                      </th>
                      <td className="px-2 py-1.5 break-all">{entry.login}</td>
                      <SumCells sum={entry} max={maxDay} />
                    </tr>
                  ))}
                </tbody>
                <tfoot>
                  <tr className="font-bold">
                    <th scope="row" colSpan={2} className="py-1.5 pr-3 text-left">
                      {t("assist.usage.total")}
                    </th>
                    <SumCells sum={sum} />
                  </tr>
                </tfoot>
              </table>
            </div>
          </div>

          <details className="rounded-control border border-hairline p-3">
            <summary className="cursor-pointer text-[13px] font-semibold">{t("assist.admin.allEntries")}</summary>
            <div className="mt-3 overflow-x-auto">
              <table className="w-full min-w-[720px] text-[13px]">
                <caption className="sr-only">{t("assist.admin.allEntries")}</caption>
                <thead>
                  <tr className="border-b border-hairline text-left text-muted">
                    <th scope="col" className="py-1.5 pr-3 font-semibold">
                      {t("assist.usage.day")}
                    </th>
                    <th scope="col" className="px-2 py-1.5 font-semibold">
                      {t("assist.usage.person")}
                    </th>
                    <th scope="col" className="px-2 py-1.5 font-semibold">
                      {t("assist.usage.provider")}
                    </th>
                    <th scope="col" className="px-2 py-1.5 font-semibold">
                      {t("assist.usage.feature")}
                    </th>
                    <SumHeads />
                  </tr>
                </thead>
                <tbody>
                  {rows.map((row, index) => (
                    <tr key={index} className="border-b border-hairline last:border-b-0">
                      <th scope="row" className="py-1.5 pr-3 text-left font-semibold whitespace-nowrap">
                        {formatDay(row.day, i18n.language)}
                      </th>
                      <td className="px-2 py-1.5 break-all">{row.login}</td>
                      <td className="px-2 py-1.5">{row.providerName}</td>
                      <td className="px-2 py-1.5">{t(`assist.features.${row.feature}`)}</td>
                      <SumCells sum={row} />
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </details>
        </div>
      )}
      <p className="mt-3 text-[12px] text-muted">{t("assist.usage.utc")}</p>
    </Card>
  );
}

/** Server → Settings → AI assistant: what is allowed, the server's providers and what they were used for. */
export function AdminAssistPage() {
  const view = useQuery({ queryKey: adminKey, queryFn: () => api<AdminAssistView>(ADMIN_ASSIST) });
  if (view.isPending) return <Loading />;
  if (view.isError) return <LoadError error={view.error} onRetry={() => void view.refetch()} />;
  return (
    <div className="flex flex-col gap-5">
      <PolicyCard policy={view.data.policy} />
      <ProvidersCard view={view.data} />
      <UsageCard />
    </div>
  );
}
