import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ShieldAlert } from "lucide-react";
import { useState } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Field, Segmented, TextInput } from "@/components/ui/Field";
import { ChoiceField, LockedHint, Section, TextField, ToggleField, type Form } from "@/features/settings/SettingsPage";
import { useT } from "@/i18n";
import { api, type LokiStatus, type SettingsView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatNumber, formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";

export const LOKI_SETTING_KEYS = [
  "log.loki.enabled",
  "log.loki.privacy_consent",
  "log.loki.url",
  "log.loki.username",
  "log.loki.password",
  "log.loki.token",
  "log.loki.tenant",
  "log.loki.labels",
  "log.loki.level",
  "log.loki.gateway",
];

type Auth = "none" | "basic" | "token";

/** A password or token: never shown, only whether one is set; typing replaces it. */
function SecretField({ form, settingKey, label }: { form: Form; settingKey: string; label: string }) {
  const { t } = useT();
  const locked = form.locked(settingKey);
  const stored = form.setting(settingKey);
  const draft = form.value(settingKey);
  return (
    <Field
      label={label}
      hint={locked ? <LockedHint /> : stored?.set && draft !== null ? t("logs.loki.secretSet") : undefined}
    >
      {(id) => (
        <div className="flex gap-2">
          <TextInput
            id={id}
            type="password"
            autoComplete="new-password"
            disabled={locked}
            placeholder={stored?.set ? "••••••••" : ""}
            value={typeof draft === "string" ? draft : ""}
            onChange={(event) => form.set(settingKey, event.target.value === "" ? undefined : event.target.value)}
          />
          {stored?.set && !locked && (
            <Button onClick={() => form.set(settingKey, null)}>{t("logs.loki.secretRemove")}</Button>
          )}
        </div>
      )}
    </Field>
  );
}

function LokiFields({ form }: { form: Form }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const enabled = Boolean(form.value("log.loki.enabled"));
  const consent = Boolean(form.value("log.loki.privacy_consent"));
  const [auth, setAuth] = useState<Auth>(() =>
    form.setting("log.loki.token")?.set ? "token" : form.setting("log.loki.username")?.value ? "basic" : "none",
  );
  const status = useQuery({
    queryKey: ["admin", "loki"],
    queryFn: () => api<LokiStatus>("/api/admin/logs/loki"),
    refetchInterval: 10_000,
  });
  const test = useMutation({
    mutationFn: () => api("/api/admin/logs/loki/test", { method: "POST", body: { changes: form.pending } }),
    onSuccess: () => toast(t("logs.loki.testOk"), "success"),
    onError: (error) => toast(errorText(error), "error"),
  });

  const chooseAuth = (next: Auth) => {
    setAuth(next);
    // Only one way of logging in is sent; the other one's values go.
    if (next !== "basic") {
      form.set("log.loki.username", null);
      form.set("log.loki.password", null);
    }
    if (next !== "token") form.set("log.loki.token", null);
  };
  const labels = (form.value("log.loki.labels") as string[] | null) ?? [];
  const locked = form.locked("log.loki.labels");
  const state = status.data;

  return (
    <>
      <div className={form.locked("log.loki.enabled") ? "pointer-events-none opacity-60" : undefined}>
        <ToggleField
          form={{
            ...form,
            // Switching off takes the agreement back: switching on again asks for it again.
            set: (key, value) => {
              form.set(key, value);
              if (value === false) form.set("log.loki.privacy_consent", false);
            },
          }}
          settingKey="log.loki.enabled"
          label={t("logs.loki.enabled")}
          hint={t("logs.loki.enabledHint")}
        />
      </div>

      {enabled && (
        <div className="flex flex-col gap-3 rounded-control bg-warning-tint px-4 py-3">
          <p className="flex items-center gap-2 text-sm font-semibold">
            <ShieldAlert className="size-4 text-warning" aria-hidden />
            {t("logs.loki.privacyTitle")}
          </p>
          <p className="text-[13px] text-muted">{t("logs.loki.privacyText")}</p>
          <label className="flex items-start gap-2 text-sm">
            <input
              type="checkbox"
              className="mt-0.5 size-4 shrink-0 accent-pink"
              checked={consent}
              disabled={form.locked("log.loki.privacy_consent")}
              onChange={(event) => form.set("log.loki.privacy_consent", event.target.checked)}
            />
            <span className="font-semibold">{t("logs.loki.privacyConsent")}</span>
          </label>
          {form.locked("log.loki.privacy_consent") && <LockedHint />}
        </div>
      )}

      <TextField form={form} settingKey="log.loki.url" label={t("logs.loki.url")} hint={t("logs.loki.urlHint")} />

      <Field label={t("logs.loki.auth")}>
        {() => (
          <Segmented<Auth>
            label={t("logs.loki.auth")}
            value={auth}
            onChange={chooseAuth}
            options={[
              { value: "none", label: t("logs.loki.authNone") },
              { value: "basic", label: t("logs.loki.authBasic") },
              { value: "token", label: t("logs.loki.authToken") },
            ]}
          />
        )}
      </Field>
      {auth === "basic" && (
        <div className="grid gap-4 sm:grid-cols-2">
          <TextField form={form} settingKey="log.loki.username" label={t("logs.loki.username")} />
          <SecretField form={form} settingKey="log.loki.password" label={t("logs.loki.password")} />
        </div>
      )}
      {auth === "token" && <SecretField form={form} settingKey="log.loki.token" label={t("logs.loki.token")} />}

      <div className="grid gap-4 sm:grid-cols-2">
        <ChoiceField
          form={form}
          settingKey="log.loki.level"
          label={t("logs.loki.level")}
          hint={t("logs.loki.levelHint")}
          options={["error", "warn", "info", "debug"].map((value) => ({
            value,
            label: t(`logs.loki.levels.${value}`),
          }))}
        />
        <TextField
          form={form}
          settingKey="log.loki.tenant"
          label={t("logs.loki.tenant")}
          hint={t("logs.loki.tenantHint")}
        />
      </div>
      <Field label={t("logs.loki.labels")} hint={locked ? <LockedHint /> : t("logs.loki.labelsHint")}>
        {(id) => (
          <textarea
            id={id}
            rows={2}
            disabled={locked}
            placeholder="env=production"
            className="w-full rounded-control border border-line bg-surface px-3.5 py-2.5 font-mono text-[13px] focus:border-pink focus:shadow-focus focus:outline-none disabled:opacity-60"
            value={labels.join("\n")}
            onChange={(event) =>
              form.set(
                "log.loki.labels",
                event.target.value
                  .split(/\s+/)
                  .map((line) => line.trim())
                  .filter(Boolean),
              )
            }
          />
        )}
      </Field>
      <ToggleField
        form={form}
        settingKey="log.loki.gateway"
        label={t("logs.loki.gateway")}
        hint={t("logs.loki.gatewayHint")}
      />

      <div className="flex flex-wrap items-center gap-3 border-t border-hairline pt-4">
        <Button busy={test.isPending} onClick={() => test.mutate()}>
          {t("logs.loki.test")}
        </Button>
        {state && (
          <p className="text-[13px] text-muted">
            {!state.enabled
              ? t("logs.loki.status.off")
              : t("logs.loki.status.sent", {
                  count: state.sent,
                  sent: formatNumber(state.sent, i18n.language),
                  when: state.lastSuccess
                    ? formatRelative(state.lastSuccess, i18n.language)
                    : t("logs.loki.status.never"),
                })}
            {state.enabled && state.queued > 0 && (
              <> · {t("logs.loki.status.queued", { queued: formatNumber(state.queued, i18n.language) })}</>
            )}
            {state.dropped > 0 && (
              <> · {t("logs.loki.status.dropped", { dropped: formatNumber(state.dropped, i18n.language) })}</>
            )}
          </p>
        )}
      </div>
      {state?.error && (
        <p className="rounded-control bg-danger-tint px-3 py-2 text-[13px] text-danger">
          {t("logs.loki.status.error", { error: state.error })}
        </p>
      )}
    </>
  );
}

/** Sending the log, and the gateway's with it, to a Grafana Loki. */
export function LokiCard() {
  const { t } = useT();
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: ["admin", "settings"], queryFn: () => api<SettingsView>("/api/admin/settings") });
  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  return (
    <Section
      title={t("logs.loki.title")}
      intro={t("logs.loki.intro")}
      view={query.data}
      keys={LOKI_SETTING_KEYS}
      canSave={(form) => !form.value("log.loki.enabled") || Boolean(form.value("log.loki.privacy_consent"))}
      onSaved={() => void queryClient.invalidateQueries({ queryKey: ["admin", "loki"] })}
    >
      {(form) => <LokiFields form={form} />}
    </Section>
  );
}
