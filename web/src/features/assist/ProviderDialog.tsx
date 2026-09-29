import { useQueryClient } from "@tanstack/react-query";
import { ExternalLink, PlugZap } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { ConfirmDiscardDialog } from "@/components/ui/ConfirmDiscardDialog";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Segmented, Select, TextInput, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatNumber } from "@/lib/format";
import { toast } from "@/state/toasts";
import { ChatgptLogin } from "./ChatgptLogin";
import {
  FEATURES,
  changeKind,
  draftOf,
  emptyDraft,
  parseLimit,
  providerBody,
  validateDraft,
  type Access,
  type AdminProvider,
  type AssistProvider,
  type DraftErrors,
  type KindInfo,
  type ModelsAnswer,
  type ProviderDraft,
} from "./model";
import { ChipInput, ExperimentalBadge, ModelPicker, Notice } from "./parts";

type AnyProvider = AssistProvider | AdminProvider;

interface Props {
  /** "admin" adds who may use it, for what and how much. */
  mode: "account" | "admin";
  kinds: KindInfo[];
  /** The provider being edited; none when one is added. */
  provider?: AnyProvider;
  /** Where the providers live: `…/providers`. */
  basePath: string;
  /** Loaded again after every change. */
  queryKey: readonly unknown[];
  /** Only for people: whether their own provider may point into the local network. */
  privateAllowed?: boolean;
  /** Only for the admin: what the chips for domains and people suggest. */
  domainSuggestions?: string[];
  peopleSuggestions?: string[];
}

function ProviderForm({
  mode,
  kinds,
  provider,
  basePath,
  queryKey,
  privateAllowed,
  domainSuggestions = [],
  peopleSuggestions = [],
  onClose,
  onCancel,
  onDirtyChange,
}: Props & { onClose: () => void; onCancel: () => void; onDirtyChange: (dirty: boolean) => void }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const admin = mode === "admin";
  /** What is stored: the provider being edited, or the new one once a test or login saved it. */
  const [current, setCurrent] = useState<AnyProvider | undefined>(provider);
  const [draft, setDraft] = useState<ProviderDraft>(() => (provider ? draftOf(provider) : emptyDraft(kinds[0])));
  const [dirty, setDirty] = useState(false);
  const [checked, setChecked] = useState(false);
  const [busy, setBusy] = useState<"save" | "test" | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [models, setModels] = useState<ModelsAnswer | null>(null);

  const kind = kinds.find((candidate) => candidate.kind === draft.kind);
  const errors: DraftErrors = checked ? validateDraft(draft, kind, { hasKey: current?.hasKey ?? false, admin }) : {};
  const errorOf = (field: keyof DraftErrors) => {
    const code = errors[field];
    return code ? t(`assist.form.errors.${code}`) : undefined;
  };

  const markDirty = (next: boolean) => {
    setDirty(next);
    onDirtyChange(next);
  };
  const change = <K extends keyof ProviderDraft>(key: K, value: ProviderDraft[K]) => {
    setDraft((old) => ({ ...old, [key]: value }));
    markDirty(true);
  };

  /**
   * Stores the form when it has something to store and answers what is stored now, or nothing
   * when the form still has a mistake. A test and the ChatGPT login need a stored provider.
   */
  const persist = async (): Promise<AnyProvider | null> => {
    setChecked(true);
    setProblem(null);
    const found = validateDraft(draft, kind, { hasKey: current?.hasKey ?? false, admin });
    if (Object.keys(found).length > 0) return null;
    if (current && !dirty) return current;
    const body = providerBody(draft, kind, { create: !current, admin });
    try {
      const saved = current
        ? await api<AnyProvider>(`${basePath}/${current.id}`, { method: "PATCH", body })
        : await api<AnyProvider>(basePath, { method: "POST", body });
      setCurrent(saved);
      setDraft((old) => ({ ...old, apiKey: "", removeKey: false }));
      markDirty(false);
      void queryClient.invalidateQueries({ queryKey });
      return saved;
    } catch (failure) {
      setProblem(errorText(failure));
      return null;
    }
  };

  const test = async () => {
    setBusy("test");
    try {
      const saved = await persist();
      if (!saved) return;
      const answer = await api<ModelsAnswer>(`${basePath}/${saved.id}/models`, { method: "POST" });
      setModels(answer);
      toast(t("assist.form.modelsLoaded", { count: answer.models.length }), "success");
    } catch (failure) {
      setProblem(errorText(failure));
    } finally {
      setBusy(null);
    }
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const wasNew = !provider;
    setBusy("save");
    const saved = await persist();
    setBusy(null);
    if (!saved) return;
    toast(wasNew ? t("assist.form.added") : t("assist.form.saved"), "success");
    onClose();
  };

  const creating = !current;
  const shownKinds = kinds.filter((candidate) => candidate.kind === draft.kind || !(admin && candidate.personalOnly));
  const keyShown = kind?.key === "required" || kind?.key === "optional";
  const defaultModel = models?.model ?? kind?.model ?? null;
  const defaultFast = models?.fastModel ?? kind?.fastModel ?? null;

  return (
    <form className="flex flex-col gap-4 px-6 pb-6" onSubmit={(event) => void submit(event)} noValidate>
      {creating ? (
        <Field label={t("assist.form.kind")}>
          {(id) => (
            <Select
              id={id}
              value={draft.kind}
              onChange={(event) => {
                const next = kinds.find((candidate) => candidate.kind === event.target.value);
                if (!next) return;
                setDraft((old) => changeKind(old, kind, next));
                setModels(null);
                markDirty(true);
              }}
            >
              {shownKinds.map((candidate) => (
                <option key={candidate.kind} value={candidate.kind}>
                  {candidate.experimental ? `${candidate.name} (${t("assist.experimental")})` : candidate.name}
                </option>
              ))}
            </Select>
          )}
        </Field>
      ) : (
        <p className="flex items-center gap-2 text-[13px] text-muted">
          {t("assist.form.kindFixed", { kind: kind?.name ?? draft.kind })}
          {kind?.experimental && <ExperimentalBadge />}
        </p>
      )}

      {kind?.kind === "anthropic" && <Notice tone="info">{t("assist.form.anthropicNote")}</Notice>}
      {kind?.experimental && (
        <Notice title={t("assist.form.chatgptWarningTitle")}>
          <p>{t("assist.form.chatgptWarning")}</p>
          <p>{t("assist.form.chatgptWarningRisk")}</p>
        </Notice>
      )}

      <Field label={t("assist.form.name")} hint={t("assist.form.nameHint")} error={errorOf("name")}>
        {(id) => (
          <TextInput
            id={id}
            maxLength={60}
            autoComplete="off"
            value={draft.name}
            onChange={(event) => change("name", event.target.value)}
          />
        )}
      </Field>

      {kind && kind.baseUrl !== "fixed" && (
        <Field
          label={kind.baseUrl === "required" ? t("assist.form.baseUrl") : t("assist.form.baseUrlOptional")}
          hint={
            <>
              {kind.baseUrl === "optional" && kind.defaultBaseUrl
                ? `${t("assist.form.baseUrlDefault", { url: kind.defaultBaseUrl })} `
                : ""}
              {admin
                ? t("assist.form.baseUrlAdmin")
                : privateAllowed
                  ? t("assist.form.baseUrlPrivate")
                  : t("assist.form.baseUrlPublic")}
            </>
          }
          error={errorOf("baseUrl")}
        >
          {(id) => (
            <TextInput
              id={id}
              type="url"
              inputMode="url"
              autoComplete="off"
              spellCheck={false}
              placeholder={
                kind.defaultBaseUrl ??
                (kind.kind === "ollama" ? "http://192.0.2.10:11434" : "https://llm.example.com/v1")
              }
              value={draft.baseUrl}
              onChange={(event) => change("baseUrl", event.target.value)}
            />
          )}
        </Field>
      )}

      {keyShown && (
        <Field
          label={kind?.key === "optional" ? t("assist.form.keyOptional") : t("assist.form.key")}
          hint={
            <span className="flex flex-col gap-1">
              <span>
                {current?.hasKey && !draft.removeKey
                  ? t("assist.form.keyStored", { hint: current.keyHint ?? "…" })
                  : draft.removeKey
                    ? t("assist.form.keyRemoved")
                    : t("assist.form.keyHint")}
              </span>
              {kind?.keyUrl && (
                <a
                  href={kind.keyUrl}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-1 self-start font-semibold text-pink hover:underline"
                >
                  <ExternalLink className="size-3.5" aria-hidden />
                  {t("assist.form.keyWhere", { kind: kind.name })}
                </a>
              )}
            </span>
          }
          error={errorOf("apiKey")}
        >
          {(id) => (
            <div className="flex gap-2">
              <TextInput
                id={id}
                type="password"
                autoComplete="new-password"
                spellCheck={false}
                placeholder={current?.hasKey && !draft.removeKey ? "••••••••" : ""}
                value={draft.apiKey}
                onChange={(event) => change("apiKey", event.target.value)}
              />
              {current?.hasKey && !draft.removeKey && kind?.key === "optional" && (
                <Button onClick={() => change("removeKey", true)}>{t("assist.form.keyRemove")}</Button>
              )}
            </div>
          )}
        </Field>
      )}

      {kind?.key === "login" && (
        <div className="flex flex-col gap-2">
          <p className="text-[13px] font-semibold text-muted">{t("assist.chatgpt.title")}</p>
          <ChatgptLogin
            basePath={basePath}
            providerId={current?.id ?? null}
            connected={current !== undefined && "connected" in current ? current.connected : false}
            prepare={async () => (await persist())?.id ?? null}
            onConnected={() => void queryClient.invalidateQueries({ queryKey })}
          />
        </div>
      )}

      <Field
        label={t("assist.form.model")}
        hint={models ? t("assist.form.modelHint") : t("assist.form.modelHintUnloaded")}
      >
        {(id) => (
          <ModelPicker
            id={id}
            value={draft.model}
            onChange={(value) => change("model", value)}
            models={models?.models ?? null}
            emptyLabel={
              defaultModel ? t("assist.form.modelDefault", { model: defaultModel }) : t("assist.form.modelNone")
            }
            placeholder={defaultModel ?? ""}
          />
        )}
      </Field>
      <Field label={t("assist.form.fastModel")} hint={t("assist.form.fastModelHint")}>
        {(id) => (
          <ModelPicker
            id={id}
            value={draft.fastModel}
            onChange={(value) => change("fastModel", value)}
            models={models?.models ?? null}
            emptyLabel={
              defaultFast ? t("assist.form.modelDefault", { model: defaultFast }) : t("assist.form.fastModelSame")
            }
            placeholder={defaultFast ?? ""}
          />
        )}
      </Field>
      <div className="flex flex-col gap-1.5">
        <div>
          <Button icon={PlugZap} busy={busy === "test"} disabled={busy === "save"} onClick={() => void test()}>
            {t("assist.form.test")}
          </Button>
        </div>
        <p className="text-[12px] text-muted">
          {creating ? t("assist.form.testSavesFirst") : t("assist.form.testHint")}
        </p>
      </div>

      {admin && (
        <AdminFields
          draft={draft}
          change={change}
          errorOf={errorOf}
          domainSuggestions={domainSuggestions}
          peopleSuggestions={peopleSuggestions}
          language={i18n.language}
        />
      )}

      {problem && (
        <p role="alert" className="rounded-control bg-danger-tint px-3 py-2 text-[13px] break-words text-danger">
          {problem}
        </p>
      )}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" type="button" onClick={onCancel}>
          {!dirty && current && !provider ? t("common.done") : t("common.cancel")}
        </Button>
        <Button variant="primary" type="submit" busy={busy === "save"} disabled={busy === "test"}>
          {creating ? t("assist.form.add") : t("common.save")}
        </Button>
      </div>
    </form>
  );
}

/** Who may use a server provider, for what and how much of it. */
function AdminFields({
  draft,
  change,
  errorOf,
  domainSuggestions,
  peopleSuggestions,
  language,
}: {
  draft: ProviderDraft;
  change: <K extends keyof ProviderDraft>(key: K, value: ProviderDraft[K]) => void;
  errorOf: (field: keyof DraftErrors) => string | undefined;
  domainSuggestions: string[];
  peopleSuggestions: string[];
  language: string;
}) {
  const { t } = useT();
  const limitHint = (text: string) => {
    const value = parseLimit(text);
    return typeof value === "number"
      ? t("assist.form.limitIs", { value: formatNumber(value, language) })
      : t("assist.form.limitHint");
  };
  return (
    <>
      <div className="border-t border-hairline pt-4">
        <Toggle
          checked={draft.enabled}
          onChange={(value) => change("enabled", value)}
          label={t("assist.form.enabled")}
          description={t("assist.form.enabledHint")}
        />
      </div>
      <div className="flex flex-col gap-2">
        <p className="text-[13px] font-semibold text-muted">{t("assist.form.access")}</p>
        <Segmented<Access>
          label={t("assist.form.access")}
          value={draft.access}
          onChange={(value) => change("access", value)}
          options={(["everyone", "domains", "people"] as const).map((value) => ({
            value,
            label: t(`assist.access.${value}`),
          }))}
        />
        {draft.access === "domains" && (
          <Field label={t("assist.form.domains")} hint={t("assist.form.domainsHint")} error={errorOf("access")}>
            {(id) => (
              <ChipInput
                id={id}
                values={draft.domains}
                onChange={(values) => change("domains", values)}
                suggestions={domainSuggestions}
                placeholder="example.com"
              />
            )}
          </Field>
        )}
        {draft.access === "people" && (
          <Field label={t("assist.form.people")} hint={t("assist.form.peopleHint")} error={errorOf("access")}>
            {(id) => (
              <ChipInput
                id={id}
                values={draft.people}
                onChange={(values) => change("people", values)}
                suggestions={peopleSuggestions}
                placeholder="leni@example.com"
              />
            )}
          </Field>
        )}
      </div>
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-2 text-[13px] font-semibold text-muted">{t("assist.form.features")}</legend>
        <div className="grid gap-2 sm:grid-cols-2">
          {FEATURES.map((feature) => (
            <label key={feature} className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                className="size-4 accent-pink"
                checked={draft.features.includes(feature)}
                onChange={(event) =>
                  change(
                    "features",
                    event.target.checked
                      ? [...draft.features, feature]
                      : draft.features.filter((other) => other !== feature),
                  )
                }
              />
              {t(`assist.features.${feature}`)}
            </label>
          ))}
        </div>
        {errorOf("features") && (
          <p role="alert" className="text-[13px] text-danger">
            {errorOf("features")}
          </p>
        )}
      </fieldset>
      <div className="flex flex-col gap-2">
        <p className="text-[13px] font-semibold text-muted">{t("assist.form.quota")}</p>
        <p className="-mt-1 text-[12px] text-muted">{t("assist.form.quotaIntro")}</p>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field
            label={t("assist.form.requestsPerDay")}
            hint={limitHint(draft.requestsPerDay)}
            error={errorOf("requestsPerDay")}
          >
            {(id) => (
              <TextInput
                id={id}
                inputMode="numeric"
                autoComplete="off"
                placeholder={t("common.unlimited")}
                value={draft.requestsPerDay}
                onChange={(event) => change("requestsPerDay", event.target.value)}
              />
            )}
          </Field>
          <Field
            label={t("assist.form.tokensPerDay")}
            hint={limitHint(draft.tokensPerDay)}
            error={errorOf("tokensPerDay")}
          >
            {(id) => (
              <TextInput
                id={id}
                inputMode="numeric"
                autoComplete="off"
                placeholder={t("common.unlimited")}
                value={draft.tokensPerDay}
                onChange={(event) => change("tokensPerDay", event.target.value)}
              />
            )}
          </Field>
        </div>
      </div>
    </>
  );
}

/** Adds or edits a provider, the admin's for the server or a person's own. */
export function ProviderDialog({ open, onClose, ...props }: Props & { open: boolean; onClose: () => void }) {
  const { t } = useT();
  const [dirty, setDirty] = useState(false);
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);
  const close = () => {
    setDirty(false);
    onClose();
  };
  const requestClose = () => {
    if (dirty) setConfirmingDiscard(true);
    else close();
  };
  return (
    <>
      <Dialog
        open={open}
        onClose={requestClose}
        closeOnOutsideClick={!dirty}
        title={props.provider ? t("assist.form.editTitle") : t("assist.form.addTitle")}
      >
        <ProviderForm {...props} onClose={close} onCancel={requestClose} onDirtyChange={setDirty} />
      </Dialog>
      <ConfirmDiscardDialog
        open={confirmingDiscard}
        onKeepEditing={() => setConfirmingDiscard(false)}
        onDiscard={() => {
          setConfirmingDiscard(false);
          close();
        }}
      />
    </>
  );
}
