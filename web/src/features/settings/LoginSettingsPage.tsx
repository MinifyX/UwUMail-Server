import { useMutation, useQuery } from "@tanstack/react-query";
import { BookOpen, PlugZap } from "lucide-react";
import { useState } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { CopyButton } from "@/components/ui/Card";
import { Field } from "@/components/ui/Field";
import { SecretField } from "@/features/logs/LokiCard";
import { useInfo } from "@/features/session/session";
import { useT } from "@/i18n";
import { api, type SettingsView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { LockedHint, Section, TextField, ToggleField, type Form } from "./SettingsPage";

const DOCS = "https://github.com/MinifyX/UwUMail-Server/blob/main/docs/login-oidc-ldap.md";
const OAUTH_DOCS = "https://github.com/MinifyX/UwUMail-Server/blob/main/docs/oauth.md";
const FETCH_DOCS = "https://github.com/MinifyX/UwUMail-Server/blob/main/docs/fetch.md#microsoft-and-google";

export const FETCH_OAUTH_SETTING_KEYS = [
  "fetch.oauth.microsoft_client_id",
  "fetch.oauth.google_client_id",
  "fetch.oauth.google_client_secret",
];

export const OIDC_SETTING_KEYS = [
  "auth.oidc.enabled",
  "auth.oidc.issuer",
  "auth.oidc.client_id",
  "auth.oidc.client_secret",
  "auth.oidc.button_label",
  "auth.oidc.auto_create",
  "auth.oidc.allowed_domains",
  "auth.oidc.admin_group_claim",
  "auth.oidc.admin_group_value",
];

export const LDAP_SETTING_KEYS = [
  "auth.ldap.enabled",
  "auth.ldap.url",
  "auth.ldap.starttls",
  "auth.ldap.insecure_localhost",
  "auth.ldap.bind_dn",
  "auth.ldap.bind_password",
  "auth.ldap.user_dn_template",
  "auth.ldap.base_dn",
  "auth.ldap.user_filter",
  "auth.ldap.mail_attribute",
  "auth.ldap.name_attribute",
  "auth.ldap.admin_group_dn",
  "auth.ldap.auto_create",
  "auth.ldap.allowed_domains",
];

interface TestResult {
  ok: boolean;
  detail: string;
  redirectUri?: string;
}

/** Domains, one per line; commas and spaces separate them too. */
function DomainsField({ form, settingKey }: { form: Form; settingKey: string }) {
  const { t } = useT();
  const locked = form.locked(settingKey);
  return (
    <Field
      label={t("externalLogin.allowedDomains")}
      hint={locked ? <LockedHint /> : t("externalLogin.allowedDomainsHint")}
    >
      {(id) => (
        <textarea
          id={id}
          rows={2}
          disabled={locked}
          placeholder="example.com"
          className="w-full rounded-control border border-line bg-surface px-3.5 py-2.5 font-mono text-[13px] focus:border-pink focus:shadow-focus focus:outline-none disabled:opacity-60"
          value={((form.value(settingKey) as string[] | null) ?? []).join("\n")}
          onChange={(event) =>
            form.set(
              settingKey,
              event.target.value
                .split(/[\s,]+/)
                .map((domain) => domain.trim().toLowerCase())
                .filter(Boolean),
            )
          }
        />
      )}
    </Field>
  );
}

/** Tries the settings as they are in the form, saved or not, and shows what came back. */
function TestButton({ path, form, onResult }: { path: string; form: Form; onResult?: (result: TestResult) => void }) {
  const { t } = useT();
  const errorText = useErrorText();
  const test = useMutation({
    mutationFn: () => api<TestResult>(path, { method: "POST", body: { changes: form.pending } }),
    onSuccess: onResult,
  });
  return (
    <div className="flex flex-col gap-3 border-t border-hairline pt-4">
      <div>
        <Button icon={PlugZap} busy={test.isPending} onClick={() => test.mutate()}>
          {t("externalLogin.test")}
        </Button>
      </div>
      {test.isSuccess && (
        <p role="status" className="rounded-control bg-success-tint px-3 py-2 text-[13px] break-words text-success">
          {t("externalLogin.testOk", { detail: test.data.detail })}
        </p>
      )}
      {test.isError && (
        <p role="alert" className="rounded-control bg-danger-tint px-3 py-2 text-[13px] break-words text-danger">
          {errorText(test.error)}
        </p>
      )}
    </div>
  );
}

function OidcFields({ form }: { form: Form }) {
  const { t } = useT();
  const info = useInfo();
  // What the server named in the last test wins; until then it is made from the host name.
  const [reported, setReported] = useState<string | null>(null);
  const redirectUri = reported ?? `https://${info.data?.hostname ?? window.location.hostname}/api/auth/oidc/callback`;
  const enabled = Boolean(form.value("auth.oidc.enabled"));
  return (
    <>
      <ToggleField
        form={form}
        settingKey="auth.oidc.enabled"
        label={t("externalLogin.oidc.enabled")}
        hint={t("externalLogin.oidc.enabledHint")}
      />
      {enabled && (
        <>
          <div className="flex flex-col gap-1 rounded-control bg-canvas px-3 py-2.5">
            <span className="text-[12px] font-semibold text-muted">{t("externalLogin.oidc.redirectUri")}</span>
            <span className="flex items-center justify-between gap-2">
              <code className="min-w-0 font-mono text-[13px] break-all select-all">{redirectUri}</code>
              <CopyButton value={redirectUri} />
            </span>
          </div>
          <TextField
            form={form}
            settingKey="auth.oidc.issuer"
            label={t("externalLogin.oidc.issuer")}
            hint={t("externalLogin.oidc.issuerHint")}
            placeholder="https://auth.example.com/application/o/uwumail/"
          />
          <div className="grid gap-4 sm:grid-cols-2">
            <TextField form={form} settingKey="auth.oidc.client_id" label={t("externalLogin.oidc.clientId")} />
            <SecretField
              form={form}
              settingKey="auth.oidc.client_secret"
              label={t("externalLogin.oidc.clientSecret")}
            />
          </div>
          <TextField
            form={form}
            settingKey="auth.oidc.button_label"
            label={t("externalLogin.oidc.buttonLabel")}
            hint={t("externalLogin.oidc.buttonLabelHint")}
            placeholder="Authentik"
            keepSpaces
          />
          <ToggleField
            form={form}
            settingKey="auth.oidc.auto_create"
            label={t("externalLogin.autoCreate")}
            hint={t("externalLogin.oidc.autoCreateHint")}
          />
          {Boolean(form.value("auth.oidc.auto_create")) && (
            <>
              <DomainsField form={form} settingKey="auth.oidc.allowed_domains" />
              <div className="grid gap-4 sm:grid-cols-2">
                <TextField
                  form={form}
                  settingKey="auth.oidc.admin_group_claim"
                  label={t("externalLogin.oidc.adminClaim")}
                  hint={t("externalLogin.oidc.adminClaimHint")}
                  placeholder="groups"
                />
                <TextField
                  form={form}
                  settingKey="auth.oidc.admin_group_value"
                  label={t("externalLogin.oidc.adminValue")}
                  hint={t("externalLogin.oidc.adminValueHint")}
                  placeholder="uwumail-admins"
                  keepSpaces
                />
              </div>
            </>
          )}
          <TestButton
            path="/api/admin/auth/oidc/test"
            form={form}
            onResult={(result) => setReported(result.redirectUri ?? null)}
          />
        </>
      )}
    </>
  );
}

function LdapFields({ form }: { form: Form }) {
  const { t } = useT();
  const enabled = Boolean(form.value("auth.ldap.enabled"));
  const url = String(form.value("auth.ldap.url") ?? "");
  const plain = url.toLowerCase().startsWith("ldap://");
  return (
    <>
      <ToggleField
        form={form}
        settingKey="auth.ldap.enabled"
        label={t("externalLogin.ldap.enabled")}
        hint={t("externalLogin.ldap.enabledHint")}
      />
      {enabled && (
        <>
          <TextField
            form={form}
            settingKey="auth.ldap.url"
            label={t("externalLogin.ldap.url")}
            hint={t("externalLogin.ldap.urlHint")}
            placeholder="ldaps://ldap.example.com"
          />
          {plain && (
            <>
              <ToggleField
                form={form}
                settingKey="auth.ldap.starttls"
                label={t("externalLogin.ldap.starttls")}
                hint={t("externalLogin.ldap.starttlsHint")}
              />
              <ToggleField
                form={form}
                settingKey="auth.ldap.insecure_localhost"
                label={t("externalLogin.ldap.insecureLocalhost")}
                hint={t("externalLogin.ldap.insecureLocalhostHint")}
              />
            </>
          )}
          <TextField
            form={form}
            settingKey="auth.ldap.user_dn_template"
            label={t("externalLogin.ldap.userDnTemplate")}
            hint={t("externalLogin.ldap.userDnTemplateHint")}
            placeholder="uid={user},ou=people,dc=example,dc=com"
            keepSpaces
          />
          {!form.value("auth.ldap.user_dn_template") && (
            <>
              <div className="grid gap-4 sm:grid-cols-2">
                <TextField
                  form={form}
                  settingKey="auth.ldap.bind_dn"
                  label={t("externalLogin.ldap.bindDn")}
                  hint={t("externalLogin.ldap.bindDnHint")}
                  placeholder="cn=uwumail,ou=services,dc=example,dc=com"
                  keepSpaces
                />
                <SecretField
                  form={form}
                  settingKey="auth.ldap.bind_password"
                  label={t("externalLogin.ldap.bindPassword")}
                />
              </div>
              <TextField
                form={form}
                settingKey="auth.ldap.base_dn"
                label={t("externalLogin.ldap.baseDn")}
                placeholder="ou=people,dc=example,dc=com"
                keepSpaces
              />
              <TextField
                form={form}
                settingKey="auth.ldap.user_filter"
                label={t("externalLogin.ldap.userFilter")}
                hint={t("externalLogin.ldap.userFilterHint")}
                placeholder="(&(objectClass=person)(mail={email}))"
                keepSpaces
              />
            </>
          )}
          <div className="grid gap-4 sm:grid-cols-2">
            <TextField
              form={form}
              settingKey="auth.ldap.mail_attribute"
              label={t("externalLogin.ldap.mailAttribute")}
              placeholder="mail"
            />
            <TextField
              form={form}
              settingKey="auth.ldap.name_attribute"
              label={t("externalLogin.ldap.nameAttribute")}
              placeholder="cn"
            />
          </div>
          <ToggleField
            form={form}
            settingKey="auth.ldap.auto_create"
            label={t("externalLogin.autoCreate")}
            hint={t("externalLogin.ldap.autoCreateHint")}
          />
          {Boolean(form.value("auth.ldap.auto_create")) && (
            <>
              <DomainsField form={form} settingKey="auth.ldap.allowed_domains" />
              <TextField
                form={form}
                settingKey="auth.ldap.admin_group_dn"
                label={t("externalLogin.ldap.adminGroupDn")}
                hint={t("externalLogin.ldap.adminGroupDnHint")}
                placeholder="cn=mail-admins,ou=groups,dc=example,dc=com"
                keepSpaces
              />
            </>
          )}
          <TestButton path="/api/admin/auth/ldap/test" form={form} />
        </>
      )}
    </>
  );
}

/** Fetched mailboxes signing in at Microsoft and Google (docs/fetch.md, "Microsoft and Google"). */
function FetchOAuthFields({ form }: { form: Form }) {
  const { t } = useT();
  const info = useInfo();
  const redirectUri = `https://${info.data?.hostname ?? window.location.hostname}/api/account/fetch/oauth/callback`;
  return (
    <>
      <TextField
        form={form}
        settingKey="fetch.oauth.microsoft_client_id"
        label={t("fetch.admin.microsoftClientId")}
        hint={t("fetch.admin.microsoftClientIdHint")}
        placeholder="f4b09124-76e0-44a5-b675-2b35a898f0d7"
      />
      <p className="text-[13px] text-muted">{t("fetch.admin.googleHint")}</p>
      <div className="flex flex-col gap-1 rounded-control bg-canvas px-3 py-2.5">
        <span className="text-[12px] font-semibold text-muted">{t("fetch.admin.redirectUri")}</span>
        <span className="flex items-center justify-between gap-2">
          <code className="min-w-0 font-mono text-[13px] break-all select-all">{redirectUri}</code>
          <CopyButton value={redirectUri} />
        </span>
      </div>
      <div className="grid gap-4 sm:grid-cols-2">
        <TextField form={form} settingKey="fetch.oauth.google_client_id" label={t("fetch.admin.googleClientId")} />
        <SecretField
          form={form}
          settingKey="fetch.oauth.google_client_secret"
          label={t("fetch.admin.googleClientSecret")}
        />
      </div>
    </>
  );
}

function DocsLink({ href, label }: { href: string; label: string }) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      className="inline-flex items-center gap-1.5 text-[13px] font-semibold text-pink-ink hover:underline"
    >
      <BookOpen className="size-4" aria-hidden />
      {label}
    </a>
  );
}

/**
 * Server → Settings → Login: logging in to the portal at an OpenID Connect provider, and checking
 * passwords at an LDAP directory (docs/login-oidc-ldap.md).
 */
export function LoginSettingsPage() {
  const { t } = useT();
  const query = useQuery({ queryKey: ["admin", "settings"], queryFn: () => api<SettingsView>("/api/admin/settings") });

  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const view = query.data;

  return (
    <div className="flex flex-col gap-5">
      <p className="rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
        {view.configFile ? t("settings.fileNote", { file: view.configFile }) : t("settings.envNote")}
      </p>
      <div className="flex flex-wrap gap-x-5 gap-y-2">
        <DocsLink href={DOCS} label={t("externalLogin.docs")} />
        <DocsLink href={OAUTH_DOCS} label={t("externalLogin.oauthDocs")} />
        <DocsLink href={FETCH_DOCS} label={t("fetch.admin.docs")} />
      </div>
      <div className="grid items-start gap-5 lg:grid-cols-2">
        <Section
          title={t("externalLogin.oidc.title")}
          intro={t("externalLogin.oidc.intro")}
          view={view}
          keys={OIDC_SETTING_KEYS}
        >
          {(form) => <OidcFields form={form} />}
        </Section>
        <Section
          title={t("externalLogin.ldap.title")}
          intro={t("externalLogin.ldap.intro")}
          view={view}
          keys={LDAP_SETTING_KEYS}
        >
          {(form) => <LdapFields form={form} />}
        </Section>
        <Section
          title={t("fetch.admin.title")}
          intro={t("fetch.admin.intro")}
          view={view}
          keys={FETCH_OAUTH_SETTING_KEYS}
        >
          {(form) => <FetchOAuthFields form={form} />}
        </Section>
      </div>
    </div>
  );
}
