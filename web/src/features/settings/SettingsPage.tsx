import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Lock } from "lucide-react";
import { useState, type ReactNode } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { Field, Segmented, Select, TextInput, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type SettingsView, type SettingValue } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { Link } from "@/lib/router";
import { usePrefs } from "@/state/prefs";
import { toast } from "@/state/toasts";

type Draft = Record<string, unknown>;
const MB = 1024 * 1024;

function LockedHint() {
  const { t } = useT();
  return (
    <span className="flex items-center gap-1 text-[12px] text-muted">
      <Lock className="size-3" aria-hidden />
      {t("settings.locked")}
    </span>
  );
}

/** A section of settings with its own draft and save button. */
export function Section({
  title,
  intro,
  view,
  keys,
  children,
  onSaved,
}: {
  title: string;
  intro: string;
  view: SettingsView;
  keys: string[];
  children: (form: Form) => ReactNode;
  onSaved?: () => void;
}) {
  const { t } = useT();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const [draft, setDraft] = useState<Draft>({});
  const byKey = Object.fromEntries(view.settings.map((setting) => [setting.key, setting]));
  const save = useMutation({
    mutationFn: (changes: Draft) => api<SettingsView>("/api/admin/settings", { method: "PATCH", body: { changes } }),
    onSuccess: (next) => {
      queryClient.setQueryData(["admin", "settings"], next);
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "domains"] });
      setDraft({});
      toast(t("settings.saved"), "success");
      onSaved?.();
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  const form: Form = {
    setting: (key) => byKey[key],
    value: (key) => (key in draft ? draft[key] : byKey[key]?.value),
    locked: (key) => byKey[key]?.source === "file",
    set: (key, value) => setDraft((current) => ({ ...current, [key]: value })),
  };
  const changes = Object.fromEntries(
    Object.entries(draft).filter(([key, value]) => {
      if (!keys.includes(key) || value === undefined) return false;
      // Secrets never come back: typing one sets it, null removes a stored one.
      if (key.endsWith("password") || key.endsWith("_key"))
        return (typeof value === "string" && value !== "") || (value === null && byKey[key]?.set);
      return JSON.stringify(value) !== JSON.stringify(byKey[key]?.value);
    }),
  );
  const dirty = Object.keys(changes).length > 0;

  return (
    <Card title={title}>
      <div className="flex flex-col gap-4">
        <p className="-mt-1 text-[13px] text-muted">{intro}</p>
        {children(form)}
        <div className="flex justify-end">
          <Button variant="primary" disabled={!dirty} busy={save.isPending} onClick={() => save.mutate(changes)}>
            {t("settings.save")}
          </Button>
        </div>
      </div>
    </Card>
  );
}

export interface Form {
  setting: (key: string) => SettingValue | undefined;
  value: (key: string) => unknown;
  locked: (key: string) => boolean;
  set: (key: string, value: unknown) => void;
}

function ChoiceField<T extends string>({
  form,
  settingKey,
  label,
  hint,
  options,
  segmented,
}: {
  form: Form;
  settingKey: string;
  label: string;
  hint?: string;
  options: { value: T; label: string }[];
  segmented?: boolean;
}) {
  const locked = form.locked(settingKey);
  const value = String(form.value(settingKey) ?? "") as T;
  return (
    <Field label={label} hint={locked ? <LockedHint /> : hint}>
      {(id) =>
        segmented && !locked ? (
          <Segmented<T> label={label} value={value} onChange={(next) => form.set(settingKey, next)} options={options} />
        ) : (
          <Select
            id={id}
            value={value}
            disabled={locked}
            onChange={(event) => form.set(settingKey, event.target.value)}
          >
            {options.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </Select>
        )
      }
    </Field>
  );
}

export function ToggleField({
  form,
  settingKey,
  label,
  hint,
}: {
  form: Form;
  settingKey: string;
  label: string;
  hint: string;
}) {
  const locked = form.locked(settingKey);
  return (
    <div className={locked ? "pointer-events-none opacity-60" : undefined}>
      <Toggle
        checked={Boolean(form.value(settingKey))}
        onChange={(checked) => form.set(settingKey, checked)}
        label={label}
        description={locked ? <LockedHint /> : hint}
      />
    </div>
  );
}

function NumberField({
  form,
  settingKey,
  label,
  hint,
  scale = 1,
}: {
  form: Form;
  settingKey: string;
  label: string;
  hint?: string;
  scale?: number;
}) {
  const locked = form.locked(settingKey);
  const raw = form.value(settingKey);
  const shown = typeof raw === "number" ? Math.round(raw / scale) : "";
  return (
    <Field label={label} hint={locked ? <LockedHint /> : hint}>
      {(id) => (
        <TextInput
          id={id}
          type="number"
          inputMode="numeric"
          disabled={locked}
          value={shown}
          onChange={(event) =>
            form.set(settingKey, event.target.value === "" ? null : Math.round(Number(event.target.value) * scale))
          }
        />
      )}
    </Field>
  );
}

/** A number with decimals, e.g. a spam score. Empty means the setting's default. */
function DecimalField({
  form,
  settingKey,
  label,
  hint,
  placeholder,
}: {
  form: Form;
  settingKey: string;
  label: string;
  hint?: string;
  placeholder?: string;
}) {
  const locked = form.locked(settingKey);
  const raw = form.value(settingKey);
  return (
    <Field label={label} hint={locked ? <LockedHint /> : hint}>
      {(id) => (
        <TextInput
          id={id}
          type="number"
          inputMode="decimal"
          min={1}
          max={100}
          step={0.5}
          disabled={locked}
          placeholder={placeholder}
          value={typeof raw === "number" ? raw : ""}
          onChange={(event) => form.set(settingKey, event.target.value === "" ? null : Number(event.target.value))}
        />
      )}
    </Field>
  );
}

function TextField({
  form,
  settingKey,
  label,
  hint,
}: {
  form: Form;
  settingKey: string;
  label: string;
  hint?: string;
}) {
  const locked = form.locked(settingKey);
  return (
    <Field label={label} hint={locked ? <LockedHint /> : hint}>
      {(id) => (
        <TextInput
          id={id}
          disabled={locked}
          autoCapitalize="none"
          spellCheck={false}
          value={String(form.value(settingKey) ?? "")}
          onChange={(event) =>
            form.set(settingKey, event.target.value.trim() === "" ? null : event.target.value.trim())
          }
        />
      )}
    </Field>
  );
}

export function SettingsPage() {
  const { t } = useT();
  const pro = usePrefs((s) => s.mode) === "pro";
  const query = useQuery({ queryKey: ["admin", "settings"], queryFn: () => api<SettingsView>("/api/admin/settings") });

  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const view = query.data;
  const choices = (values: string[], prefix: string) =>
    values.map((value) => ({ value, label: t(`${prefix}.${value}`) }));

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("settings.title")} intro={t("settings.intro")} />
      {pro && (
        <p className="rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
          {view.configFile ? t("settings.fileNote", { file: view.configFile }) : t("settings.envNote")}
        </p>
      )}

      <Section
        title={t("settings.delivery.title")}
        intro={t("settings.delivery.intro")}
        view={view}
        keys={[
          "delivery.relay.host",
          "delivery.relay.port",
          "delivery.relay.security",
          "delivery.relay.username",
          "delivery.relay.password",
          "delivery.require_tls",
          "delivery.max_lifetime_hours",
          "smtp.allow_external_forwarding",
        ]}
      >
        {(form) => <DeliveryFields form={form} pro={pro} throughGateway={view.gateway.paired} />}
      </Section>

      <Section
        title={t("settings.tone.title")}
        intro={t("settings.tone.intro")}
        view={view}
        keys={["tone.language", "tone.internal", "tone.external"]}
      >
        {(form) => (
          <>
            <ChoiceField
              form={form}
              settingKey="tone.language"
              label={t("settings.tone.language")}
              options={choices(["de", "en"], "settings.tone.options")}
            />
            <ChoiceField
              form={form}
              settingKey="tone.internal"
              label={t("settings.tone.internal")}
              hint={t("settings.tone.internalHint")}
              segmented
              options={choices(["playful", "neutral"], "settings.tone.options")}
            />
            <ChoiceField
              form={form}
              settingKey="tone.external"
              label={t("settings.tone.external")}
              hint={t("settings.tone.externalHint")}
              segmented
              options={choices(["neutral", "light"], "settings.tone.options")}
            />
          </>
        )}
      </Section>

      <p className="flex flex-wrap items-center gap-x-2 gap-y-1 rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
        {t("settings.spamHint")}
        <Link to="/admin/spam" className="font-semibold text-pink-ink hover:underline">
          {t("settings.spamLink")}
        </Link>
      </p>

      {pro && (
        <div className="grid gap-5 lg:grid-cols-2">
          <Section
            title={t("settings.receiving.title")}
            intro={t("settings.receiving.intro")}
            view={view}
            keys={["smtp.max_message_size", "smtp.max_recipients", "smtp.trusted_relays"]}
          >
            {(form) => (
              <>
                <NumberField
                  form={form}
                  settingKey="smtp.max_message_size"
                  label={t("settings.receiving.maxSize")}
                  scale={MB}
                />
                <NumberField
                  form={form}
                  settingKey="smtp.max_recipients"
                  label={t("settings.receiving.maxRecipients")}
                />
                <Field
                  label={t("settings.receiving.trustedRelays")}
                  hint={form.locked("smtp.trusted_relays") ? <LockedHint /> : t("settings.receiving.trustedRelaysHint")}
                >
                  {(id) => (
                    <textarea
                      id={id}
                      rows={3}
                      disabled={form.locked("smtp.trusted_relays")}
                      className="w-full rounded-control border border-line bg-surface px-3.5 py-2.5 font-mono text-[13px] focus:border-pink focus:shadow-focus focus:outline-none disabled:opacity-60"
                      value={((form.value("smtp.trusted_relays") as string[] | null) ?? []).join("\n")}
                      onChange={(event) =>
                        form.set(
                          "smtp.trusted_relays",
                          event.target.value
                            .split(/\s+/)
                            .map((line) => line.trim())
                            .filter(Boolean),
                        )
                      }
                    />
                  )}
                </Field>
              </>
            )}
          </Section>
          <Section
            title={t("settings.apps.title")}
            intro={t("settings.apps.intro")}
            view={view}
            keys={["smtp.require_tls_for_auth", "smtp.reveal_client_ip"]}
          >
            {(form) => (
              <>
                <ToggleField
                  form={form}
                  settingKey="smtp.require_tls_for_auth"
                  label={t("settings.apps.requireTls")}
                  hint={t("settings.apps.requireTlsHint")}
                />
                <ToggleField
                  form={form}
                  settingKey="smtp.reveal_client_ip"
                  label={t("settings.apps.revealIp")}
                  hint={t("settings.apps.revealIpHint")}
                />
              </>
            )}
          </Section>
        </div>
      )}
    </div>
  );
}

/** Every setting on the spam filter page. */
export const SPAM_LOG_SETTING_KEYS = ["spam.log.enabled", "spam.log.clean_subjects", "spam.log.retention_days"];

/** The spam history: kept at all, for how long, and whether mail that arrived keeps its subject. */
export function SpamLogFields({ form }: { form: Form }) {
  const { t } = useT();
  const enabled = Boolean(form.value("spam.log.enabled"));
  return (
    <>
      <ToggleField
        form={form}
        settingKey="spam.log.enabled"
        label={t("settings.spamLog.enabled")}
        hint={t("settings.spamLog.enabledHint")}
      />
      {enabled && (
        <>
          <ToggleField
            form={form}
            settingKey="spam.log.clean_subjects"
            label={t("settings.spamLog.cleanSubjects")}
            hint={t("settings.spamLog.cleanSubjectsHint")}
          />
          <div className="grid gap-4 sm:grid-cols-2">
            <NumberField form={form} settingKey="spam.log.retention_days" label={t("settings.spamLog.retentionDays")} />
          </div>
        </>
      )}
    </>
  );
}

export const SPAM_SETTING_KEYS = [
  "spam.enabled",
  "spam.blocklists",
  "spam.bayes",
  "spam.junk_score",
  "spam.greylist_score",
  "spam.greylist_delay_secs",
  "spam.reject_score",
  "smtp.verify_senders",
  "smtp.enforce_dmarc_reject",
];

/** The spam filter: switches for everyone; the numbers behind it and the sender checks in Pro mode. */
export function SpamFields({ form, pro }: { form: Form; pro: boolean }) {
  const { t } = useT();
  const enabled = Boolean(form.value("spam.enabled"));
  return (
    <>
      <ToggleField
        form={form}
        settingKey="spam.enabled"
        label={t("settings.spam.enabled")}
        hint={t("settings.spam.enabledHint")}
      />
      {enabled && (
        <ToggleField
          form={form}
          settingKey="spam.blocklists"
          label={t("settings.spam.blocklists")}
          hint={t("settings.spam.blocklistsHint")}
        />
      )}
      {enabled && (
        <ToggleField
          form={form}
          settingKey="spam.bayes"
          label={t("settings.spam.bayes")}
          hint={t("settings.spam.bayesHint")}
        />
      )}
      {enabled && pro && (
        <div className="grid gap-4 sm:grid-cols-2">
          <DecimalField
            form={form}
            settingKey="spam.junk_score"
            label={t("settings.spam.junkScore")}
            hint={t("settings.spam.junkScoreHint")}
          />
          <DecimalField
            form={form}
            settingKey="spam.greylist_score"
            label={t("settings.spam.greylistScore")}
            hint={t("settings.spam.greylistScoreHint")}
          />
          <NumberField
            form={form}
            settingKey="spam.greylist_delay_secs"
            label={t("settings.spam.greylistDelay")}
            hint={t("settings.spam.greylistDelayHint")}
            scale={60}
          />
          <DecimalField
            form={form}
            settingKey="spam.reject_score"
            label={t("settings.spam.rejectScore")}
            hint={t("settings.spam.rejectScoreHint")}
            placeholder={t("settings.spam.rejectScoreOff")}
          />
        </div>
      )}
      {pro && (
        <>
          <ToggleField
            form={form}
            settingKey="smtp.verify_senders"
            label={t("settings.receiving.verifySenders")}
            hint={t("settings.receiving.verifySendersHint")}
          />
          <ToggleField
            form={form}
            settingKey="smtp.enforce_dmarc_reject"
            label={t("settings.receiving.enforceDmarc")}
            hint={t("settings.receiving.enforceDmarcHint")}
          />
        </>
      )}
    </>
  );
}

/** Sending settings; `relayOnly` shows just the relay fields, for the setup assistant. */
export function DeliveryFields({
  form,
  pro,
  relayOnly = false,
  throughGateway = false,
}: {
  form: Form;
  pro: boolean;
  relayOnly?: boolean;
  /** Mail leaves through a paired UwUMail Gateway, whatever the route below says. */
  throughGateway?: boolean;
}) {
  const { t } = useT();
  const hostLocked = form.locked("delivery.relay.host");
  const [relayMode, setRelayMode] = useState(relayOnly || Boolean(form.setting("delivery.relay.host")?.value));
  const password = form.setting("delivery.relay.password");
  const passwordDraft = form.value("delivery.relay.password");

  return (
    <>
      {/* The gateway sits below the route: from the moment it is paired, everything leaves through
          it, direct or by relay. Without this line the page reads "Direct" and invites a change
          that would not do what it looks like. */}
      {throughGateway && !relayOnly && (
        <p className="flex flex-wrap items-center gap-x-2 gap-y-1 rounded-control bg-pink-tint/50 px-3 py-2 text-[13px]">
          {t("settings.delivery.gatewayNote")}
          <Link to="/admin/setup" className="font-semibold text-pink-ink hover:underline">
            {t("settings.delivery.gatewayLink")}
          </Link>
        </p>
      )}
      {!relayOnly && (
        <>
          <Field label={t("settings.delivery.mode")} hint={hostLocked ? <LockedHint /> : undefined}>
            {() =>
              hostLocked ? (
                <p className="text-sm font-semibold">{t("settings.delivery.relay")}</p>
              ) : (
                <Segmented<"direct" | "relay">
                  label={t("settings.delivery.mode")}
                  value={relayMode ? "relay" : "direct"}
                  onChange={(mode) => {
                    setRelayMode(mode === "relay");
                    if (mode === "direct") form.set("delivery.relay.host", null);
                  }}
                  options={[
                    { value: "direct", label: t("settings.delivery.direct") },
                    { value: "relay", label: t("settings.delivery.relay") },
                  ]}
                />
              )
            }
          </Field>
          <p className="-mt-2 text-[13px] text-muted">
            {relayMode || hostLocked
              ? t("settings.delivery.relayHint")
              : throughGateway
                ? t("settings.delivery.directViaGatewayHint")
                : t("settings.delivery.directHint")}
          </p>
        </>
      )}
      {(relayMode || hostLocked) && (
        <div className="grid gap-4 sm:grid-cols-2">
          <TextField form={form} settingKey="delivery.relay.host" label={t("settings.delivery.host")} />
          <NumberField form={form} settingKey="delivery.relay.port" label={t("settings.delivery.port")} />
          <ChoiceField
            form={form}
            settingKey="delivery.relay.security"
            label={t("settings.delivery.security")}
            options={["starttls", "tls", "none"].map((value) => ({
              value,
              label: t(`settings.delivery.securityOptions.${value}`),
            }))}
          />
          <TextField form={form} settingKey="delivery.relay.username" label={t("settings.delivery.username")} />
          <Field
            label={t("settings.delivery.password")}
            hint={
              form.locked("delivery.relay.password") ? (
                <LockedHint />
              ) : password?.set && passwordDraft !== null ? (
                t("settings.delivery.passwordSet")
              ) : undefined
            }
          >
            {(id) => (
              <div className="flex gap-2">
                <TextInput
                  id={id}
                  type="password"
                  autoComplete="new-password"
                  disabled={form.locked("delivery.relay.password")}
                  placeholder={password?.set ? "••••••••" : ""}
                  value={typeof passwordDraft === "string" ? passwordDraft : ""}
                  onChange={(event) =>
                    form.set("delivery.relay.password", event.target.value === "" ? undefined : event.target.value)
                  }
                />
                {password?.set && !form.locked("delivery.relay.password") && (
                  <Button onClick={() => form.set("delivery.relay.password", null)}>
                    {t("settings.delivery.passwordRemove")}
                  </Button>
                )}
              </div>
            )}
          </Field>
        </div>
      )}
      {!relayOnly && (
        <>
          <ToggleField
            form={form}
            settingKey="delivery.require_tls"
            label={t("settings.delivery.requireTls")}
            hint={t("settings.delivery.requireTlsHint")}
          />
          <ToggleField
            form={form}
            settingKey="smtp.allow_external_forwarding"
            label={t("settings.delivery.allowForwarding")}
            hint={t("settings.delivery.allowForwardingHint")}
          />
          {pro && (
            <NumberField
              form={form}
              settingKey="delivery.max_lifetime_hours"
              label={t("settings.delivery.lifetime")}
              hint={t("settings.delivery.lifetimeHint")}
            />
          )}
        </>
      )}
    </>
  );
}
