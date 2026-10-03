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
import { toast } from "@/state/toasts";
import {
  ACCOUNT_ASSIST,
  CLASSIFIER_MIN_EXAMPLES,
  FEATURES,
  byDay,
  byFeature,
  currencyFor,
  formatCost,
  formatDay,
  hasCosts,
  personalKinds,
  usageSum,
  type AccountAssistView,
  type AccountUsageView,
  type AssistProvider,
  type AssistSettings,
  type Choice,
  type Feature,
  type LabelSummary,
} from "./model";
import {
  EntriesTable,
  ExperimentalBadge,
  ModelPicker,
  Notice,
  PriceText,
  QuotaText,
  SumTable,
  Tag,
  UsageMeter,
  UsageTotals,
} from "./parts";
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
          {provider.price && (
            <span className="block text-[12px] text-muted">
              <PriceText price={provider.price} />
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
    kinds: personalKinds(view.kinds),
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

/** The person's labels with their mail and what puts them on by itself. */
function LabelList({ labels }: { labels: LabelSummary[] }) {
  const { t } = useT();
  const how = (label: LabelSummary): string => {
    const parts: string[] = [];
    if (label.hasRules) parts.push(t("assist.labels.rules"));
    if (label.detector) parts.push(t(`assist.labels.detectors.${label.detector}`));
    if (label.learnSenders) parts.push(t("assist.labels.senders"));
    if (label.classifier) {
      parts.push(
        label.examples >= CLASSIFIER_MIN_EXAMPLES
          ? t("assist.labels.classifierReady", { n: label.examples })
          : t("assist.labels.classifierLearning", { n: label.examples, min: CLASSIFIER_MIN_EXAMPLES }),
      );
    }
    return parts.join(" · ");
  };
  return (
    <ul className="flex flex-col divide-y divide-hairline rounded-lg border border-hairline text-[13px]">
      {labels.map((label) => (
        <li key={label.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2">
          <span
            className="size-2.5 shrink-0 rounded-full bg-muted"
            style={label.color ? { backgroundColor: label.color } : undefined}
            aria-hidden
          />
          <span className="min-w-0 flex-1 truncate font-semibold">{label.name}</span>
          <span className="text-muted tabular-nums">
            {t("assist.labels.counts", { total: label.totalEmails, unread: label.unreadEmails })}
          </span>
          <span className="basis-full pl-5 text-[12px] text-muted">{how(label) || t("assist.labels.byHandOnly")}</span>
        </li>
      ))}
    </ul>
  );
}

/** Labels on incoming mail, and appointments looked for when a mail is opened. */
function BackgroundCard({ view, webmail }: { view: AccountAssistView; webmail: boolean }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const settings = view.settings;
  const save = useMutation({
    mutationFn: (body: Partial<Pick<AssistSettings, "autoLabels" | "nonAiLabels" | "refineEvents">>) =>
      api<AssistSettings>(`${ACCOUNT_ASSIST}/settings`, { method: "PUT", body }),
    onSuccess: (next) => {
      queryClient.setQueryData<AccountAssistView>(assistKey, (old) => (old ? { ...old, settings: next } : old));
      toast(t("common.saved"), "success");
    },
    onError: (failure) => toast(errorText(failure), "error"),
  });
  const pending = save.isPending ? save.variables : undefined;
  const autoLabels = pending?.autoLabels ?? settings.autoLabels;
  const nonAiLabels = pending?.nonAiLabels ?? settings.nonAiLabels ?? true;
  const refineEvents = pending?.refineEvents ?? settings.refineEvents;

  return (
    <Card title={t("assist.background.title")}>
      <div className="flex flex-col gap-4">
        <Toggle
          checked={nonAiLabels}
          onChange={(value) => save.mutate({ nonAiLabels: value })}
          label={t("assist.background.nonAiLabels")}
          description={t("assist.background.nonAiLabelsHint")}
        />
        {view.labelList && view.labelList.length > 0 && <LabelList labels={view.labelList} />}
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
        {view.foreignMail && <p className="text-[13px] text-muted">{t("assist.background.foreignMail")}</p>}
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

/** In English, euros or US dollars for the costs: the synced user setting `assist.currency`. */
function CurrencyChoice({ view }: { view: AccountAssistView }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const save = useMutation({
    mutationFn: (currency: "EUR" | "USD") =>
      api<AssistSettings>(`${ACCOUNT_ASSIST}/settings`, { method: "PUT", body: { currency } }),
    onSuccess: (next) => {
      queryClient.setQueryData<AccountAssistView>(assistKey, (old) => (old ? { ...old, settings: next } : old));
      void queryClient.invalidateQueries({ queryKey: usageKey });
    },
    onError: (failure) => toast(errorText(failure), "error"),
  });
  const value = save.isPending ? save.variables : view.settings.currency === "USD" ? "USD" : "EUR";
  return (
    <Field label={t("assist.usage.currency")} className="w-full sm:w-56">
      {(id) => (
        <Select id={id} value={value} onChange={(event) => save.mutate(event.target.value as "EUR" | "USD")}>
          <option value="EUR">{t("assist.usage.currencyEur")}</option>
          <option value="USD">{t("assist.usage.currencyUsd")}</option>
        </Select>
      )}
    </Field>
  );
}

/** What was asked of which provider today and in the last 30 days, and what it cost. */
function UsageCard({ view }: { view: AccountAssistView }) {
  const { t, i18n } = useT();
  const currency = currencyFor(i18n.language, view.settings.currency);
  const usage = useQuery({
    queryKey: [...usageKey, currency],
    queryFn: () => api<AccountUsageView>(`${ACCOUNT_ASSIST}/usage?days=30&currency=${currency}`),
  });
  if (usage.isPending) return <Loading />;
  if (usage.isError) return <LoadError error={usage.error} onRetry={() => void usage.refetch()} />;
  const rows = usage.data.days;
  const today = usage.data.today;
  const costs = hasCosts(rows) || today.some((entry) => entry.cost != null) ? currency : undefined;
  const english = i18n.language.toLowerCase().startsWith("en");

  return (
    <Card title={t("assist.usage.title")}>
      <div className="flex flex-col gap-4">
        {english && <CurrencyChoice view={view} />}
        <div className="flex flex-col gap-2">
          <p className="text-[13px] font-semibold text-muted">{t("assist.usage.today")}</p>
          {today.length === 0 ? (
            <p className="text-[13px] text-muted">{t("assist.usage.todayEmpty")}</p>
          ) : (
            <ul className="flex flex-col gap-3">
              {today.map((entry) => (
                <li key={entry.providerId} className="flex flex-col gap-1.5">
                  <span className="flex flex-wrap items-baseline justify-between gap-2">
                    <span className="text-sm font-semibold">{entry.providerName}</span>
                    {entry.cost && (
                      <span className="text-[13px] text-muted tabular-nums">
                        {t("assist.usage.costToday", {
                          cost: formatCost(entry.cost.amount, entry.cost.currency, i18n.language),
                        })}
                      </span>
                    )}
                  </span>
                  <div className="grid gap-2 sm:grid-cols-2">
                    <UsageMeter used={entry.requests} limit={entry.requestsPerDay} label={t("assist.usage.requests")} />
                    <UsageMeter used={entry.tokens} limit={entry.tokensPerDay} label={t("assist.usage.tokens")} />
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>
        <div className="flex flex-col gap-3 border-t border-hairline pt-4">
          <p className="text-[13px] font-semibold text-muted">{t("assist.usage.last30")}</p>
          {rows.length === 0 ? (
            <p className="text-[13px] text-muted">{t("assist.usage.empty")}</p>
          ) : (
            <>
              <UsageTotals sum={usageSum(rows)} currency={costs} />
              <SumTable
                caption={t("assist.usage.perFeature")}
                head={t("assist.usage.feature")}
                rows={byFeature(rows)}
                rowKey={(entry) => entry.feature}
                label={(entry) => t(`assist.features.${entry.feature}`)}
                currency={costs}
              />
              <details className="rounded-control border border-hairline p-3">
                <summary className="cursor-pointer text-[13px] font-semibold">{t("assist.usage.perDay")}</summary>
                <div className="mt-3">
                  <SumTable
                    caption={t("assist.usage.perDay")}
                    head={t("assist.usage.day")}
                    rows={byDay(rows)}
                    rowKey={(entry) => entry.day}
                    label={(entry) => <span className="whitespace-nowrap">{formatDay(entry.day, i18n.language)}</span>}
                    total={usageSum(rows)}
                    currency={costs}
                  />
                </div>
              </details>
              <EntriesTable rows={rows} showPerson={false} currency={costs} />
            </>
          )}
          <p className="text-[12px] text-muted">{t("assist.usage.utc")}</p>
          {costs && <p className="text-[12px] text-muted">{t("assist.usage.costNote")}</p>}
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
      <UsageCard view={view.data} />
    </div>
  );
}
