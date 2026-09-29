import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, CircleSlash, ExternalLink, LogIn, Plus, ShieldCheck, Trash2 } from "lucide-react";
import { useState } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { EmptyState } from "@/components/ui/EmptyState";
import { Field, Select, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatNumber } from "@/lib/format";
import { toast } from "@/state/toasts";
import {
  ACCOUNT_ASSIST,
  FEATURES,
  formatDay,
  usageSum,
  type AccountAssistView,
  type AccountUsageView,
  type AssistProvider,
  type AssistSettings,
  type Choice,
  type Feature,
} from "./model";
import { ModelPicker, ExperimentalBadge, Notice, QuotaText, Tag, UsageMeter } from "./parts";
import { ProviderDialog } from "./ProviderDialog";

const assistKey = ["account", "assist"] as const;
const usageKey = ["account", "assist", "usage"] as const;

/** Which features can be used right now. */
function FeaturesCard({ view }: { view: AccountAssistView }) {
  const { t } = useT();
  const none = FEATURES.every((feature) => !view.features[feature]);
  return (
    <Card title={t("assist.account.featuresTitle")}>
      {none && (
        <div className="mb-3">
          <Notice tone="info">
            {view.mayAddProviders ? t("assist.account.noneAddOwn") : t("assist.account.noneAskAdmin")}
          </Notice>
        </div>
      )}
      <ul className="flex flex-col">
        {FEATURES.map((feature) => {
          const on = view.features[feature];
          const effective = view.settings.effective[feature];
          return (
            <li key={feature} className="flex items-start gap-3 border-b border-hairline py-2.5 last:border-b-0">
              {on ? (
                <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-success" aria-hidden />
              ) : (
                <CircleSlash className="mt-0.5 size-4 shrink-0 text-faint" aria-hidden />
              )}
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-semibold">
                  {t(`assist.features.${feature}`)}
                  <span className="sr-only">
                    {" "}
                    — {on ? t("assist.account.available") : t("assist.account.unavailable")}
                  </span>
                </span>
                <span className="block text-[12px] text-muted">{t(`assist.featureHints.${feature}`)}</span>
                {on && effective && (
                  <span className="mt-0.5 block text-[12px] font-semibold break-words text-ink">
                    {t("assist.account.uses", { provider: effective.providerName, model: effective.model })}
                  </span>
                )}
              </span>
              {!on && <Tag tone="muted">{t("assist.account.unavailable")}</Tag>}
            </li>
          );
        })}
      </ul>
    </Card>
  );
}

function ProviderRow({
  provider,
  view,
  onEdit,
  onRemove,
}: {
  provider: AssistProvider;
  view: AccountAssistView;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const { t } = useT();
  const kind = view.kinds.find((candidate) => candidate.kind === provider.kind);
  const today = view.today.find((entry) => entry.providerId === provider.id);
  const own = provider.scope === "personal";
  return (
    <li className="flex flex-col gap-2 border-b border-hairline py-3 last:border-b-0">
      <div className="flex flex-wrap items-center gap-3">
        <span className="min-w-0 flex-1">
          <span className="flex flex-wrap items-center gap-1.5">
            <span className="truncate text-sm font-semibold">{provider.name}</span>
            <Tag tone={own ? "pink" : "plain"}>{t(`assist.scope.${provider.scope}`)}</Tag>
            {provider.experimental && <ExperimentalBadge />}
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
          {own && provider.baseUrl && (
            <span className="block truncate font-mono text-[12px] text-muted">{provider.baseUrl}</span>
          )}
          {own && provider.hasKey && provider.keyHint && (
            <span className="block text-[12px] text-muted">{t("assist.provider.key", { hint: provider.keyHint })}</span>
          )}
          {!own && (
            <span className="block text-[12px] text-muted">
              <QuotaText quota={provider.quota} />
            </span>
          )}
          {!provider.connected && (
            <span className="block text-[12px] font-semibold text-warning">
              {provider.kind === "chatgpt" ? t("assist.provider.notSignedIn") : t("assist.provider.noKey")}
            </span>
          )}
        </span>
        {own && (
          <div className="flex items-center gap-1.5">
            {provider.kind === "chatgpt" && !provider.connected && (
              <Button size="sm" icon={LogIn} onClick={onEdit}>
                {t("assist.chatgpt.signIn")}
              </Button>
            )}
            <Button size="sm" variant="ghost" onClick={onEdit}>
              {t("assist.provider.edit")}
            </Button>
            <IconButton icon={Trash2} label={t("assist.provider.remove")} onClick={onRemove} />
          </div>
        )}
      </div>
      {today && (today.requestsPerDay !== null || today.tokensPerDay !== null) && (
        <div className="grid gap-2 sm:grid-cols-2">
          <UsageMeter used={today.requests} limit={today.requestsPerDay} label={t("assist.usage.requestsToday")} />
          <UsageMeter used={today.tokens} limit={today.tokensPerDay} label={t("assist.usage.tokensToday")} />
        </div>
      )}
    </li>
  );
}

function ProvidersCard({ view }: { view: AccountAssistView }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<AssistProvider | null>(null);
  const own = view.providers.filter((provider) => provider.scope === "personal");
  const full = own.length >= view.maxProviders;
  const remove = useMutation({
    mutationFn: (id: number) => api<void>(`${ACCOUNT_ASSIST}/providers/${id}`, { method: "DELETE" }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: assistKey });
      toast(t("assist.provider.removed"), "success");
    },
    onError: (failure) => toast(errorText(failure), "error"),
  });
  const dialogProps = {
    mode: "account" as const,
    kinds: view.kinds,
    basePath: `${ACCOUNT_ASSIST}/providers`,
    queryKey: assistKey,
    privateAllowed: view.mayUsePrivateAddresses,
  };

  return (
    <Card title={t("assist.account.providersTitle")}>
      <div className="flex flex-col gap-4">
        {view.providers.length === 0 ? (
          <EmptyState
            compact
            scene="addons"
            title={t("assist.account.providersEmptyTitle")}
            body={view.mayAddProviders ? t("assist.account.providersEmptyOwn") : t("assist.account.providersEmpty")}
          />
        ) : (
          <ul className="flex flex-col">
            {view.providers.map((provider) => (
              <ProviderRow
                key={provider.id}
                provider={provider}
                view={view}
                onEdit={() => setEditing(provider)}
                onRemove={() => {
                  if (window.confirm(t("assist.provider.removeConfirm", { name: provider.name }))) {
                    remove.mutate(provider.id);
                  }
                }}
              />
            ))}
          </ul>
        )}
        {view.mayAddProviders ? (
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p className="min-w-0 flex-1 basis-60 text-[12px] text-muted">
              {full ? t("assist.account.providersFull", { max: view.maxProviders }) : t("assist.account.providersHint")}
            </p>
            <Button variant="primary" icon={Plus} disabled={full} onClick={() => setAdding(true)}>
              {t("assist.account.addProvider")}
            </Button>
          </div>
        ) : (
          <p className="text-[12px] text-muted">{t("assist.account.ownNotAllowed")}</p>
        )}
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

type ChoiceDraft = Pick<AssistSettings, "default" | "features">;

/** A provider and optionally a model for one feature, or for all of them. */
function ChoiceRow({
  label,
  hint,
  value,
  onChange,
  providers,
  emptyLabel,
  feature,
}: {
  label: string;
  hint?: string;
  value: Choice | null;
  onChange: (choice: Choice | null) => void;
  providers: AssistProvider[];
  emptyLabel: string;
  feature?: Feature;
}) {
  const { t } = useT();
  const chosen = value ? providers.find((provider) => provider.id === value.providerId) : undefined;
  // What the provider uses when no model is named: `model` for writing, `fastModel` for the rest.
  const standard = chosen ? (feature === "compose" ? chosen.model : (chosen.fastModel ?? chosen.model)) : null;
  const models = chosen
    ? [...new Set([chosen.model, chosen.fastModel].filter((model): model is string => Boolean(model)))].map((id) => ({
        id,
        name: id,
      }))
    : [];
  return (
    <div className="grid gap-3 sm:grid-cols-2">
      <Field label={label} hint={hint}>
        {(id) => (
          <Select
            id={id}
            value={value ? String(value.providerId) : ""}
            onChange={(event) =>
              onChange(event.target.value ? { providerId: Number(event.target.value), model: null } : null)
            }
          >
            <option value="">{emptyLabel}</option>
            {providers.map((provider) => (
              <option key={provider.id} value={provider.id}>
                {`${provider.name} (${t(`assist.scope.${provider.scope}`)})`}
              </option>
            ))}
            {value && !chosen && <option value={value.providerId}>{t("assist.choices.gone")}</option>}
          </Select>
        )}
      </Field>
      {value && (
        <Field label={t("assist.choices.model")}>
          {(id) => (
            <ModelPicker
              id={id}
              value={value.model ?? ""}
              onChange={(model) => onChange({ ...value, model: model.trim() ? model : null })}
              models={models}
              emptyLabel={
                standard ? t("assist.form.modelDefault", { model: standard }) : t("assist.choices.modelStandard")
              }
              placeholder={standard ?? ""}
            />
          )}
        </Field>
      )}
    </div>
  );
}

function ChoicesCard({ view }: { view: AccountAssistView }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const settings = view.settings;
  // Nothing typed yet shows what is stored; saving goes back to that.
  const [draft, setDraft] = useState<ChoiceDraft | null>(null);
  const shown: ChoiceDraft = draft ?? { default: settings.default, features: settings.features };
  const usable = view.providers;
  const save = useMutation({
    mutationFn: (body: ChoiceDraft) => api<AssistSettings>(`${ACCOUNT_ASSIST}/settings`, { method: "PUT", body }),
    onSuccess: (next) => {
      queryClient.setQueryData<AccountAssistView>(assistKey, (old) => (old ? { ...old, settings: next } : old));
      setDraft(null);
      toast(t("assist.choices.saved"), "success");
    },
    onError: (failure) => toast(errorText(failure), "error"),
  });

  return (
    <Card title={t("assist.choices.title")}>
      <p className="-mt-1 mb-4 text-[13px] text-muted">{t("assist.choices.intro")}</p>
      <div className="flex flex-col gap-5">
        <ChoiceRow
          label={t("assist.choices.default")}
          hint={t("assist.choices.defaultHint")}
          value={shown.default}
          onChange={(choice) => setDraft({ ...shown, default: choice })}
          providers={usable}
          emptyLabel={t("assist.choices.automatic")}
        />
        {FEATURES.map((feature) => {
          const effective = settings.effective[feature];
          return (
            <div key={feature} className="flex flex-col gap-1.5 border-t border-hairline pt-4">
              <ChoiceRow
                label={t(`assist.features.${feature}`)}
                value={shown.features[feature]}
                onChange={(choice) => setDraft({ ...shown, features: { ...shown.features, [feature]: choice } })}
                providers={usable.filter((provider) => provider.features.includes(feature))}
                emptyLabel={t("assist.choices.likeDefault")}
                feature={feature}
              />
              <p className="text-[12px] text-muted">
                {effective
                  ? t("assist.choices.effective", {
                      provider: effective.providerName,
                      model: effective.model,
                      scope: t(`assist.scope.${effective.scope}`),
                    })
                  : t("assist.choices.effectiveNone")}
              </p>
            </div>
          );
        })}
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

/** Labels on incoming mail, and appointments looked for when a mail is opened. */
function BackgroundCard({ view, webmail }: { view: AccountAssistView; webmail: boolean }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const settings = view.settings;
  const save = useMutation({
    mutationFn: (body: Partial<Pick<AssistSettings, "autoLabels" | "refineEvents">>) =>
      api<AssistSettings>(`${ACCOUNT_ASSIST}/settings`, { method: "PUT", body }),
    onSuccess: (next) => {
      queryClient.setQueryData<AccountAssistView>(assistKey, (old) => (old ? { ...old, settings: next } : old));
      toast(t("common.saved"), "success");
    },
    onError: (failure) => toast(errorText(failure), "error"),
  });
  const pending = save.isPending ? save.variables : undefined;
  const autoLabels = pending?.autoLabels ?? settings.autoLabels;
  const refineEvents = pending?.refineEvents ?? settings.refineEvents;

  return (
    <Card title={t("assist.background.title")}>
      <div className="flex flex-col gap-4">
        <Toggle
          checked={autoLabels}
          disabled={!view.features.autoLabels && !settings.autoLabels}
          onChange={(value) => save.mutate({ autoLabels: value })}
          label={t("assist.background.autoLabels")}
          description={t("assist.background.autoLabelsHint")}
        />
        <div className="flex flex-col gap-2 text-[13px] text-muted">
          {!view.features.autoLabels ? (
            <p>{t("assist.background.autoLabelsUnavailable")}</p>
          ) : view.labels === 0 ? (
            <Notice tone={autoLabels ? "warning" : "info"}>{t("assist.background.noLabels")}</Notice>
          ) : (
            <p>{t("assist.background.labels", { count: view.labels })}</p>
          )}
          {webmail && (
            <a
              href="/mail"
              className="inline-flex items-center gap-1 self-start font-semibold text-pink hover:underline"
            >
              <ExternalLink className="size-3.5" aria-hidden />
              {t("assist.background.openWebmail")}
            </a>
          )}
        </div>
        <div className="border-t border-hairline pt-4">
          <Toggle
            checked={refineEvents}
            disabled={!view.features.extractEvents && !settings.refineEvents}
            onChange={(value) => save.mutate({ refineEvents: value })}
            label={t("assist.background.refineEvents")}
            description={t("assist.background.refineEventsHint")}
          />
        </div>
      </div>
    </Card>
  );
}

/** What was asked of which provider in the last 30 days. */
function UsageCard() {
  const { t, i18n } = useT();
  const usage = useQuery({
    queryKey: usageKey,
    queryFn: () => api<AccountUsageView>(`${ACCOUNT_ASSIST}/usage?days=30`),
  });
  const number = (value: number) => formatNumber(value, i18n.language);
  if (usage.isPending) return <Loading />;
  if (usage.isError) return <LoadError error={usage.error} onRetry={() => void usage.refetch()} />;
  const rows = usage.data.days;
  const sum = usageSum(rows);
  const today = usage.data.today;

  return (
    <Card title={t("assist.usage.title")}>
      <div className="flex flex-col gap-4">
        <div className="flex flex-col gap-2">
          <p className="text-[13px] font-semibold text-muted">{t("assist.usage.today")}</p>
          {today.length === 0 ? (
            <p className="text-[13px] text-muted">{t("assist.usage.todayEmpty")}</p>
          ) : (
            <ul className="flex flex-col gap-3">
              {today.map((entry) => (
                <li key={entry.providerId} className="flex flex-col gap-1.5">
                  <span className="text-sm font-semibold">{entry.providerName}</span>
                  <div className="grid gap-2 sm:grid-cols-2">
                    <UsageMeter used={entry.requests} limit={entry.requestsPerDay} label={t("assist.usage.requests")} />
                    <UsageMeter used={entry.tokens} limit={entry.tokensPerDay} label={t("assist.usage.tokens")} />
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>
        <div className="flex flex-col gap-2 border-t border-hairline pt-4">
          <p className="text-[13px] font-semibold text-muted">{t("assist.usage.last30")}</p>
          {rows.length === 0 ? (
            <p className="text-[13px] text-muted">{t("assist.usage.empty")}</p>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[520px] text-[13px]">
                <caption className="sr-only">{t("assist.usage.caption")}</caption>
                <thead>
                  <tr className="border-b border-hairline text-left text-muted">
                    <th scope="col" className="py-1.5 pr-3 font-semibold">
                      {t("assist.usage.day")}
                    </th>
                    <th scope="col" className="px-2 py-1.5 font-semibold">
                      {t("assist.usage.provider")}
                    </th>
                    <th scope="col" className="px-2 py-1.5 font-semibold">
                      {t("assist.usage.feature")}
                    </th>
                    <th scope="col" className="px-2 py-1.5 text-right font-semibold">
                      {t("assist.usage.requests")}
                    </th>
                    <th scope="col" className="px-2 py-1.5 text-right font-semibold">
                      {t("assist.usage.inputTokens")}
                    </th>
                    <th scope="col" className="py-1.5 pl-2 text-right font-semibold">
                      {t("assist.usage.outputTokens")}
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((row, index) => (
                    <tr key={index} className="border-b border-hairline">
                      <th scope="row" className="py-1.5 pr-3 text-left font-semibold whitespace-nowrap">
                        {formatDay(row.day, i18n.language)}
                      </th>
                      <td className="px-2 py-1.5">{row.providerName}</td>
                      <td className="px-2 py-1.5">{t(`assist.features.${row.feature}`)}</td>
                      <td className="px-2 py-1.5 text-right tabular-nums">{number(row.requests)}</td>
                      <td className="px-2 py-1.5 text-right tabular-nums">{number(row.inputTokens)}</td>
                      <td className="py-1.5 pl-2 text-right tabular-nums">{number(row.outputTokens)}</td>
                    </tr>
                  ))}
                </tbody>
                <tfoot>
                  <tr className="font-bold">
                    <th scope="row" colSpan={3} className="py-1.5 pr-3 text-left">
                      {t("assist.usage.total")}
                    </th>
                    <td className="px-2 py-1.5 text-right tabular-nums">{number(sum.requests)}</td>
                    <td className="px-2 py-1.5 text-right tabular-nums">{number(sum.inputTokens)}</td>
                    <td className="py-1.5 pl-2 text-right tabular-nums">{number(sum.outputTokens)}</td>
                  </tr>
                </tfoot>
              </table>
            </div>
          )}
          <p className="text-[12px] text-muted">{t("assist.usage.utc")}</p>
        </div>
      </div>
    </Card>
  );
}

function PrivacyCard() {
  const { t } = useT();
  return (
    <Card title={t("assist.privacy.title")}>
      <ul className="flex flex-col gap-2 text-[13px] text-muted">
        {(["server", "sent", "never", "limits"] as const).map((key) => (
          <li key={key} className="flex items-start gap-2">
            <ShieldCheck className="mt-0.5 size-4 shrink-0 text-pink" aria-hidden />
            {t(`assist.privacy.${key}`)}
          </li>
        ))}
      </ul>
    </Card>
  );
}

/** Account → AI assistant: which providers, what for, and how much was used. */
export function AccountAssistPage({ webmail = false }: { webmail?: boolean }) {
  const { t } = useT();
  const view = useQuery({ queryKey: assistKey, queryFn: () => api<AccountAssistView>(ACCOUNT_ASSIST) });
  if (view.isPending) return <Loading />;
  if (view.isError) return <LoadError error={view.error} onRetry={() => void view.refetch()} />;
  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("assist.title")} intro={t("assist.intro")} />
      <div className="grid gap-5 lg:grid-cols-2">
        <FeaturesCard view={view.data} />
        <PrivacyCard />
      </div>
      <ProvidersCard view={view.data} />
      <ChoicesCard view={view.data} />
      <BackgroundCard view={view.data} webmail={webmail} />
      <UsageCard />
    </div>
  );
}
