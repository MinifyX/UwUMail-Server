import { useQuery } from "@tanstack/react-query";
import { KeyRound } from "lucide-react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { CopyButton } from "@/components/ui/Card";
import { Field, TextInput } from "@/components/ui/Field";
import { LockedHint, Section, ToggleField, type Form } from "@/features/settings/SettingsPage";
import { useT } from "@/i18n";
import { api, type SettingsView } from "@/lib/api";

export const METRICS_SETTING_KEYS = ["metrics.enabled", "metrics.token", "metrics.allowed_networks"];

const TOKEN_ALPHABET = "abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/** 32 random characters (about 185 bits), made in the browser and only ever sent to the server. */
export function newMetricsToken(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  // 256 is not a multiple of the alphabet's length; the small bias this leaves costs a bit or two.
  return Array.from(bytes, (byte) => TOKEN_ALPHABET[byte % TOKEN_ALPHABET.length]).join("");
}

/** The lines of the networks field as the setting's list. */
export function parseNetworks(text: string): string[] {
  return text
    .split(/[\s,]+/)
    .map((line) => line.trim())
    .filter(Boolean);
}

/**
 * The token as drafted: a new one (only then is it ever seen), the stored one, or none. The server
 * never sends a stored token back, only that there is one.
 */
export function tokenState(form: Form): { kind: "fresh"; token: string } | { kind: "stored" } | { kind: "none" } {
  const draft = form.value("metrics.token");
  if (typeof draft === "string" && draft !== "") return { kind: "fresh", token: draft };
  if (form.pending["metrics.token"] === null) return { kind: "none" };
  return form.setting("metrics.token")?.set ? { kind: "stored" } : { kind: "none" };
}

/** Whether the metrics may be switched on as drafted: never without a token or a network. */
export function metricsCanSave(form: Form): boolean {
  if (!form.value("metrics.enabled")) return true;
  const networks = (form.value("metrics.allowed_networks") as string[] | null) ?? [];
  return tokenState(form).kind !== "none" || networks.length > 0;
}

function MetricsFields({ form }: { form: Form }) {
  const { t } = useT();
  const url = `${window.location.origin}/metrics`;
  const tokenLocked = form.locked("metrics.token");
  const token = tokenState(form);
  const fresh = token.kind === "fresh" ? token.token : null;
  const networksLocked = form.locked("metrics.allowed_networks");
  const networks = (form.value("metrics.allowed_networks") as string[] | null) ?? [];
  const enabled = Boolean(form.value("metrics.enabled"));

  return (
    <>
      <ToggleField
        form={form}
        settingKey="metrics.enabled"
        label={t("stats.metrics.enabled")}
        hint={t("stats.metrics.enabledHint")}
      />
      <Field
        label={t("stats.metrics.token")}
        hint={
          tokenLocked ? (
            <LockedHint />
          ) : fresh ? (
            t("stats.metrics.tokenFresh")
          ) : token.kind === "stored" ? (
            t("stats.metrics.tokenSet")
          ) : (
            t("stats.metrics.tokenHint")
          )
        }
      >
        {(id) => (
          <div className="flex flex-wrap gap-2">
            <TextInput
              id={id}
              readOnly
              disabled={tokenLocked}
              className="min-w-0 flex-1 basis-60 font-mono text-[13px]"
              placeholder={token.kind === "stored" ? "••••••••" : ""}
              value={fresh ?? ""}
            />
            {fresh && <CopyButton value={fresh} label={t("stats.metrics.copyToken")} />}
            {!tokenLocked && (
              <Button icon={KeyRound} onClick={() => form.set("metrics.token", newMetricsToken())}>
                {token.kind === "none" ? t("stats.metrics.tokenNew") : t("stats.metrics.tokenReplace")}
              </Button>
            )}
            {!tokenLocked && token.kind !== "none" && (
              <Button onClick={() => form.set("metrics.token", form.setting("metrics.token")?.set ? null : undefined)}>
                {t("stats.metrics.tokenRemove")}
              </Button>
            )}
          </div>
        )}
      </Field>
      <Field
        label={t("stats.metrics.networks")}
        hint={networksLocked ? <LockedHint /> : t("stats.metrics.networksHint")}
      >
        {(id) => (
          <textarea
            id={id}
            rows={2}
            disabled={networksLocked}
            placeholder="192.0.2.0/24"
            className="w-full rounded-control border border-line bg-surface px-3.5 py-2.5 font-mono text-[13px] focus:border-pink focus:shadow-focus focus:outline-none disabled:opacity-60"
            value={networks.join("\n")}
            onChange={(event) => form.set("metrics.allowed_networks", parseNetworks(event.target.value))}
          />
        )}
      </Field>
      {enabled && !metricsCanSave(form) && (
        <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px]">{t("stats.metrics.needsWayIn")}</p>
      )}
      <div className="flex flex-col gap-1.5 rounded-control bg-canvas px-3 py-2 text-[13px]">
        <span className="text-muted">{t("stats.metrics.address")}</span>
        <span className="flex items-center gap-2">
          <code className="min-w-0 flex-1 font-mono break-all">{url}</code>
          <CopyButton value={url} />
        </span>
      </div>
    </>
  );
}

/** Prometheus under Server → Statistics: off by default, with a token and optionally only from some networks. */
export function MetricsCard() {
  const { t } = useT();
  const query = useQuery({ queryKey: ["admin", "settings"], queryFn: () => api<SettingsView>("/api/admin/settings") });
  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  return (
    <Section
      title={t("stats.metrics.title")}
      intro={t("stats.metrics.intro")}
      view={query.data}
      keys={METRICS_SETTING_KEYS}
      canSave={metricsCanSave}
    >
      {(form) => <MetricsFields form={form} />}
    </Section>
  );
}
