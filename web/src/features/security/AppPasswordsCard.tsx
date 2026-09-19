import { KeySquare, Plus, Trash2 } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { ConfirmCloseSecretDialog } from "@/components/ui/ConfirmCloseSecretDialog";
import { ConfirmDiscardDialog } from "@/components/ui/ConfirmDiscardDialog";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Select, TextInput, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type AppPasswordInfo, type AppScope, type SecurityView, type Session } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDate, formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";
import { Cancelled } from "./ConfirmPassword";
import { useSecurityAction } from "./queries";
import { CopyTextButton, SecretBox } from "./SecurityBits";

type Confirmed = <T>(action: (password?: string) => Promise<T>) => Promise<T>;

const DAY = 86_400;
const EXPIRY_CHOICES = [0, 30, 90, 365];

interface Created {
  appPassword: AppPasswordInfo;
  secret: string;
}

function CreateForm({
  confirmed,
  onCreated,
  onDirtyChange,
  usable,
}: {
  confirmed: Confirmed;
  onCreated: (created: Created) => void;
  onDirtyChange: (dirty: boolean) => void;
  usable: AppScope[];
}) {
  const { t } = useT();
  const errorText = useErrorText();
  const [name, setName] = useState("");
  const [scopes, setScopes] = useState<AppScope[]>(() => usable.filter((scope) => scope !== "dav"));
  const [days, setDays] = useState(0);
  const [nowAtOpen] = useState(() => Math.floor(Date.now() / 1000));
  const start = usable.filter((scope) => scope !== "dav");
  const dirty = name.trim() !== "" || days !== 0 || [...scopes].sort().join() !== [...start].sort().join();
  useEffect(() => {
    onDirtyChange(dirty);
    return () => onDirtyChange(false);
  }, [dirty, onDirtyChange]);
  const create = useSecurityAction(() =>
    confirmed((password) =>
      api<Created>("/api/account/app-passwords", {
        method: "POST",
        body: { name, scopes, expiresAt: days > 0 ? nowAtOpen + days * DAY : null, password },
      }),
    ),
  );
  const toggle = (scope: AppScope, on: boolean) =>
    setScopes((current) => (on ? [...new Set([...current, scope])] : current.filter((s) => s !== scope)));
  const submit = (event: FormEvent) => {
    event.preventDefault();
    create.mutate(undefined, { onSuccess: onCreated });
  };
  const failed = create.isError && !(create.error instanceof Cancelled);

  return (
    <form className="flex flex-col gap-4 px-6 pt-1 pb-6" onSubmit={submit}>
      <Field label={t("security.appPasswords.name")} hint={t("security.appPasswords.nameHint")}>
        {(id) => (
          <TextInput
            id={id}
            autoFocus
            required
            maxLength={60}
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
        )}
      </Field>
      <fieldset className="flex flex-col gap-3">
        <legend className="mb-1 text-[13px] font-semibold text-muted">{t("security.appPasswords.scopes")}</legend>
        {usable.includes("mail") && (
          <Toggle
            checked={scopes.includes("mail")}
            onChange={(on) => toggle("mail", on)}
            label={t("security.appPasswords.scopeMail")}
            description={t("security.appPasswords.scopeMailHint")}
          />
        )}
        {usable.includes("smtp") && (
          <Toggle
            checked={scopes.includes("smtp")}
            onChange={(on) => toggle("smtp", on)}
            label={t("security.appPasswords.scopeSmtp")}
            description={t("security.appPasswords.scopeSmtpHint")}
          />
        )}
        {usable.includes("dav") && (
          <Toggle
            checked={scopes.includes("dav")}
            onChange={(on) => toggle("dav", on)}
            label={t("security.appPasswords.scopeDav")}
            description={t("security.appPasswords.scopeDavHint")}
          />
        )}
      </fieldset>
      <Field label={t("security.appPasswords.expiry")}>
        {(id) => (
          <Select id={id} value={days} onChange={(event) => setDays(Number(event.target.value))}>
            {EXPIRY_CHOICES.map((choice) => (
              <option key={choice} value={choice}>
                {choice === 0 ? t("security.appPasswords.never") : t("security.appPasswords.days", { count: choice })}
              </option>
            ))}
          </Select>
        )}
      </Field>
      {failed && (
        <p role="alert" className="text-[13px] text-danger">
          {errorText(create.error)}
        </p>
      )}
      <div className="flex justify-end">
        <Button type="submit" variant="primary" busy={create.isPending} disabled={scopes.length === 0 || !name.trim()}>
          {t("security.appPasswords.create")}
        </Button>
      </div>
    </form>
  );
}

function CreatedView({ created, login, onClose }: { created: Created; login: string; onClose: () => void }) {
  const { t } = useT();
  return (
    <div className="flex flex-col gap-4 px-6 pt-1 pb-6">
      <p className="text-sm text-muted">{t("security.appPasswords.createdBody", { name: created.appPassword.name })}</p>
      <SecretBox login={login} secret={created.secret} />
      <div className="flex flex-wrap justify-between gap-2">
        <CopyTextButton value={created.secret} label={t("security.appPasswords.copy")} />
        <Button variant="primary" onClick={onClose}>
          {t("security.appPasswords.done")}
        </Button>
      </div>
    </div>
  );
}

export function AppPasswordRow({ appPassword, onRevoke }: { appPassword: AppPasswordInfo; onRevoke: () => void }) {
  const { t, i18n } = useT();
  const language = i18n.language;
  const [nowAtRender] = useState(() => Math.floor(Date.now() / 1000));
  const expired = appPassword.expiresAt !== null && appPassword.expiresAt <= nowAtRender;
  const used = appPassword.lastUsedAt
    ? t("security.appPasswords.lastUsed", {
        time: formatRelative(appPassword.lastUsedAt, language),
        protocol: (appPassword.lastUsedProtocol ?? "").toUpperCase(),
        ip: appPassword.lastUsedIp ?? "",
      })
    : t("security.appPasswords.neverUsed");
  return (
    <li className="flex items-start gap-3 border-b border-hairline py-3 last:border-b-0">
      <KeySquare className="mt-0.5 size-4 shrink-0 text-muted" aria-hidden />
      <span className="min-w-0 flex-1">
        <span className="flex flex-wrap items-center gap-1.5">
          <span className="truncate text-sm font-semibold">{appPassword.name}</span>
          {appPassword.scopes.map((scope) => (
            <span
              key={scope}
              className="rounded-full bg-pink-tint px-2 text-[11px] leading-5 font-semibold text-pink-ink"
            >
              {t(`security.appPasswords.scopeShort.${scope}`)}
            </span>
          ))}
          {expired && (
            <span className="rounded-full bg-warning-tint px-2 text-[11px] leading-5 font-semibold text-warning">
              {t("security.appPasswords.expired")}
            </span>
          )}
        </span>
        <span className="block text-[12px] text-muted">{used}</span>
        <span className="block text-[12px] text-faint">
          {t("security.appPasswords.created", { date: formatDate(appPassword.createdAt, language) })}
          {appPassword.expiresAt !== null &&
            !expired &&
            ` · ${t("security.appPasswords.expires", { date: formatDate(appPassword.expiresAt, language) })}`}
        </span>
      </span>
      <Button size="sm" variant="danger" icon={Trash2} onClick={onRevoke}>
        {t("security.appPasswords.revoke")}
      </Button>
    </li>
  );
}

export function AppPasswordsCard({
  security,
  session,
  confirmed,
}: {
  security: SecurityView;
  session: Session;
  confirmed: Confirmed;
}) {
  const { t } = useT();
  const errorText = useErrorText();
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<Created | null>(null);
  const [revoking, setRevoking] = useState<AppPasswordInfo | null>(null);
  const [dirty, setDirty] = useState(false);
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);
  const [confirmingSecret, setConfirmingSecret] = useState(false);

  const closeCreate = () => {
    setCreating(false);
    setCreated(null);
  };
  const requestCloseCreate = () => {
    // The new password is on screen once and nowhere else, so leaving is worth a question too.
    if (created) setConfirmingSecret(true);
    else if (dirty) setConfirmingDiscard(true);
    else closeCreate();
  };

  const failure = (error: unknown) => {
    if (!(error instanceof Cancelled)) toast(errorText(error), "error");
  };
  const setRequired = useSecurityAction((on: boolean) =>
    confirmed((password) =>
      api<void>("/api/account/apps-need-app-password", { method: "PUT", body: { on, password } }),
    ),
  );
  const revoke = useSecurityAction((id: number) => api<void>(`/api/account/app-passwords/${id}`, { method: "DELETE" }));

  return (
    <Card
      title={t("security.appPasswords.title")}
      action={
        <Button size="sm" icon={Plus} onClick={() => setCreating(true)}>
          {t("security.appPasswords.new")}
        </Button>
      }
    >
      <div className="flex flex-col gap-4">
        {security.secondFactor ? (
          <p className="rounded-control bg-pink-tint/60 px-3 py-2.5 text-[13px] text-pink-ink">
            {t("security.appPasswords.requiredBySecondFactor")}
          </p>
        ) : (
          <Toggle
            checked={security.appsNeedAppPassword}
            onChange={(on) => setRequired.mutate(on, { onError: failure })}
            label={t("security.appPasswords.requireLabel")}
            description={t("security.appPasswords.requireHint")}
          />
        )}
        {security.appPasswords.length === 0 ? (
          <p className="text-sm text-muted">{t("security.appPasswords.none")}</p>
        ) : (
          <ul className="flex flex-col">
            {security.appPasswords.map((appPassword) => (
              <AppPasswordRow
                key={appPassword.id}
                appPassword={appPassword}
                onRevoke={() => setRevoking(appPassword)}
              />
            ))}
          </ul>
        )}
      </div>

      <Dialog
        open={creating || created !== null}
        onClose={requestCloseCreate}
        closeOnOutsideClick={!dirty && created === null}
        title={created ? t("security.appPasswords.createdTitle") : t("security.appPasswords.newTitle")}
      >
        {created ? (
          <CreatedView created={created} login={session.account.login} onClose={closeCreate} />
        ) : (
          creating && (
            <CreateForm
              confirmed={confirmed}
              usable={security.appPasswordScopes}
              onDirtyChange={setDirty}
              onCreated={(result) => {
                setCreated(result);
                setCreating(false);
              }}
            />
          )
        )}
      </Dialog>

      <ConfirmDiscardDialog
        open={confirmingDiscard}
        onKeepEditing={() => setConfirmingDiscard(false)}
        onDiscard={() => {
          setConfirmingDiscard(false);
          closeCreate();
        }}
      />

      <ConfirmCloseSecretDialog
        open={confirmingSecret}
        onBack={() => setConfirmingSecret(false)}
        onClose={() => {
          setConfirmingSecret(false);
          closeCreate();
        }}
      />

      <Dialog
        open={revoking !== null}
        onClose={() => setRevoking(null)}
        title={t("security.appPasswords.revokeTitle", { name: revoking?.name ?? "" })}
        width="sm"
      >
        <div className="flex flex-col gap-4 px-6 pt-1 pb-6">
          <p className="text-sm text-muted">{t("security.appPasswords.revokeBody")}</p>
          <div className="flex justify-end gap-2">
            <Button onClick={() => setRevoking(null)}>{t("common.cancel")}</Button>
            <Button
              variant="danger"
              icon={Trash2}
              busy={revoke.isPending}
              onClick={() =>
                revoking &&
                revoke.mutate(revoking.id, {
                  onSuccess: () => {
                    toast(t("security.appPasswords.revoked", { name: revoking.name }), "success");
                    setRevoking(null);
                  },
                  onError: failure,
                })
              }
            >
              {t("security.appPasswords.revoke")}
            </Button>
          </div>
        </div>
      </Dialog>
    </Card>
  );
}
