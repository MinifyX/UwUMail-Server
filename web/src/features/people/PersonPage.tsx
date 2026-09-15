import clsx from "clsx";
import { ArrowLeft, KeyRound, Lock, LockOpen, Plus, RotateCcw, ShieldCheck, ShieldOff, Trash2, X } from "lucide-react";
import { useState, type FormEvent, type ReactNode } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { Card, KeyValue } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Select, TextInput, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import type { PasswordLinkCreated, Person, Session } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDate } from "@/lib/format";
import { Link, navigate } from "@/lib/router";
import { usePrefs } from "@/state/prefs";
import { toast } from "@/state/toasts";
import { LinkBox, QuotaSelect } from "./CreatePersonDialog";
import { AdminPill, PersonAvatar, StatusPill, StorageLine } from "./PersonBits";
import {
  useAddAlias,
  useCreatePasswordLink,
  useDomains,
  usePerson,
  usePurgePerson,
  useRemoveAlias,
  useResetSecondFactors,
  useSetAliasLimit,
  useSetExternalForwarding,
  useRestorePerson,
  useSetPassword,
  useTrashPerson,
  useUpdatePerson,
} from "./queries";

function Banner({
  tone,
  children,
  action,
}: {
  tone: "pink" | "warning" | "danger";
  children: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div
      className={clsx(
        "flex flex-wrap items-center justify-between gap-3 rounded-card px-4 py-3 text-sm font-medium",
        tone === "pink" && "bg-pink-tint text-pink-ink",
        tone === "warning" && "bg-warning-tint text-warning",
        tone === "danger" && "bg-danger-tint text-danger",
      )}
    >
      <span>{children}</span>
      {action}
    </div>
  );
}

function NameField({ person }: { person: Person }) {
  const { t } = useT();
  const [name, setName] = useState(person.name);
  const update = useUpdatePerson(person.login, () => t("people.toasts.saved"));
  const changed = name.trim() !== person.name;
  return (
    <form
      className="flex items-end gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (changed) update.mutate({ name: name.trim() });
      }}
    >
      <Field label={t("people.detail.name")} className="flex-1">
        {(id) => <TextInput id={id} value={name} onChange={(event) => setName(event.target.value)} />}
      </Field>
      {changed && (
        <Button type="submit" variant="primary" busy={update.isPending}>
          {t("common.save")}
        </Button>
      )}
    </form>
  );
}

const ALIAS_LIMITS = [0, 3, 5, 10, 25, 50];

function AliasLimit({ person }: { person: Person }) {
  const { t } = useT();
  const errorText = useErrorText();
  const save = useSetAliasLimit(person.login);
  if (person.aliasLimit === undefined) return null;
  const options = ALIAS_LIMITS.includes(person.aliasLimit)
    ? ALIAS_LIMITS
    : [...ALIAS_LIMITS, person.aliasLimit].sort((a, b) => a - b);
  return (
    <Field label={t("people.detail.aliasLimit")} hint={t("people.detail.aliasLimitHint")} className="mt-3">
      {(id) => (
        <Select
          id={id}
          value={person.aliasLimit}
          disabled={save.isPending}
          onChange={(event) =>
            save.mutate(Number(event.target.value), { onError: (error) => toast(errorText(error), "error") })
          }
        >
          {options.map((limit) => (
            <option key={limit} value={limit}>
              {limit === 0 ? t("people.detail.aliasLimitNone") : t("people.detail.aliasLimitCount", { count: limit })}
            </option>
          ))}
        </Select>
      )}
    </Field>
  );
}

function Addresses({ person, editable }: { person: Person; editable: boolean }) {
  const { t } = useT();
  const domains = useDomains();
  const [localPart, setLocalPart] = useState("");
  const [domain, setDomain] = useState("");
  const add = useAddAlias(person.login, (address) => t("people.toasts.aliasAdded", { address }));
  const remove = useRemoveAlias(person.login, (address) => t("people.toasts.aliasRemoved", { address }));
  const chosenDomain = domain || domains.data?.[0]?.name || "";

  const submit = (event: FormEvent) => {
    event.preventDefault();
    add.mutate(`${localPart.trim()}@${chosenDomain}`, { onSuccess: () => setLocalPart("") });
  };

  return (
    <Card title={t("people.detail.addresses")}>
      <ul className="flex flex-col">
        {person.addresses.map((address) => (
          <li
            key={address.address}
            className="flex min-h-11 items-center justify-between gap-3 border-b border-hairline last:border-b-0"
          >
            <span className="min-w-0">
              <span className="block truncate text-sm font-semibold">{address.address}</span>
              <span className="block text-[12px] text-muted">
                {address.kind === "primary" ? t("people.detail.primary") : t("people.detail.alias")}
              </span>
            </span>
            {editable && address.kind === "alias" && (
              <IconButton
                size="sm"
                icon={X}
                label={t("people.detail.removeAlias", { address: address.address })}
                onClick={() => remove.mutate(address.address)}
              />
            )}
          </li>
        ))}
      </ul>
      {editable && (
        <form className="mt-3 flex flex-wrap items-center gap-2" onSubmit={submit}>
          <TextInput
            required
            aria-label={t("people.create.localPart")}
            placeholder={t("people.create.localPart")}
            className="h-10 min-w-0 flex-1 basis-32"
            autoCapitalize="none"
            spellCheck={false}
            value={localPart}
            onChange={(event) => setLocalPart(event.target.value.replace(/[@\s]/g, ""))}
          />
          {/* "@" and the domain stay together when the row wraps on phones. */}
          <span className="flex min-w-0 flex-1 basis-44 items-center gap-2">
            <span className="text-muted">@</span>
            {(domains.data?.length ?? 0) > 1 ? (
              <Select
                aria-label={t("people.create.domain")}
                className="min-w-0 flex-1 [&_select]:h-10"
                value={chosenDomain}
                onChange={(event) => setDomain(event.target.value)}
              >
                {domains.data?.map((entry) => (
                  <option key={entry.name} value={entry.name}>
                    {entry.name}
                  </option>
                ))}
              </Select>
            ) : (
              <span className="text-sm font-semibold">{chosenDomain}</span>
            )}
          </span>
          <Button type="submit" icon={Plus} busy={add.isPending}>
            {t("people.detail.addAlias")}
          </Button>
        </form>
      )}
      {editable && <AliasLimit person={person} />}
    </Card>
  );
}

function Access({ person }: { person: Person }) {
  const { t } = useT();
  const pro = usePrefs((s) => s.mode) === "pro";
  const errorText = useErrorText();
  const createLink = useCreatePasswordLink(person.login);
  const setPassword = useSetPassword(person.login);
  const [link, setLink] = useState<PasswordLinkCreated | null>(null);
  const [password, setPasswordValue] = useState("");

  return (
    <Card title={t("people.detail.access")}>
      <p className="mb-3 text-[13px] text-muted">{t("people.detail.accessHint")}</p>
      {link ? (
        <LinkBox link={link} />
      ) : (
        <Button
          icon={KeyRound}
          busy={createLink.isPending}
          onClick={() => createLink.mutate(undefined, { onSuccess: setLink })}
        >
          {person.status === "invited" ? t("people.detail.createInvite") : t("people.detail.createLink")}
        </Button>
      )}
      {pro && (
        <form
          className="mt-5 flex flex-col gap-2 border-t border-hairline pt-4"
          onSubmit={(event) => {
            event.preventDefault();
            setPassword.mutate(password, {
              onSuccess: () => {
                setPasswordValue("");
                toast(t("people.toasts.passwordSet"), "success");
              },
            });
          }}
        >
          <Field
            label={t("people.detail.setPassword")}
            hint={t("people.detail.setPasswordHint")}
            error={setPassword.isError && errorText(setPassword.error)}
          >
            {(id) => (
              <div className="flex gap-2">
                <TextInput
                  id={id}
                  type="password"
                  autoComplete="new-password"
                  minLength={10}
                  required
                  value={password}
                  onChange={(event) => setPasswordValue(event.target.value)}
                />
                <Button type="submit" busy={setPassword.isPending}>
                  {t("common.save")}
                </Button>
              </div>
            )}
          </Field>
        </form>
      )}
    </Card>
  );
}

/** What protects the login, and a way out for someone who lost their second factor. */
function SecurityInfo({ person, isMe }: { person: Person; isMe: boolean }) {
  const { t } = useT();
  const errorText = useErrorText();
  const reset = useResetSecondFactors(person.login);
  const externalForwarding = useSetExternalForwarding(person.login);
  const [asking, setAsking] = useState(false);
  const security = person.security;
  if (!security) return null;
  const methods = [
    security.totp && t("people.security.totp"),
    security.passkeys > 0 && t("people.security.passkeys", { count: security.passkeys }),
  ].filter(Boolean);

  return (
    <Card title={t("people.security.title")}>
      <div className="flex flex-col gap-3">
        <p className="flex items-start gap-2 text-sm">
          {security.secondFactor ? (
            <ShieldCheck className="mt-0.5 size-4 shrink-0 text-success" aria-hidden />
          ) : (
            <ShieldOff className="mt-0.5 size-4 shrink-0 text-muted" aria-hidden />
          )}
          <span>
            {security.secondFactor
              ? t("people.security.on", { methods: methods.join(", ") })
              : t("people.security.off")}
          </span>
        </p>
        <p className="text-[13px] text-muted">
          {t("people.security.appPasswords", { count: security.appPasswords })}
          {security.appPasswordsRequired && ` · ${t("people.security.appsOnly")}`}
        </p>
        {security.secondFactor && !isMe && (
          <div className="flex flex-col items-start gap-2 border-t border-hairline pt-3 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
            <p className="min-w-0 flex-1 text-[13px] text-muted">{t("people.security.resetHint")}</p>
            <Button icon={ShieldOff} onClick={() => setAsking(true)}>
              {t("people.security.reset")}
            </Button>
          </div>
        )}
        {person.forwarding && (
          <div className="flex flex-col gap-2 border-t border-hairline pt-3">
            <Toggle
              checked={!person.forwarding.externalBlocked}
              onChange={(allowed) =>
                externalForwarding.mutate(!allowed, { onError: (error) => toast(errorText(error), "error") })
              }
              label={t("people.forwarding.allowExternal")}
              description={
                person.forwarding.targets > 0
                  ? t("people.forwarding.targets", { count: person.forwarding.targets })
                  : t("people.forwarding.none")
              }
            />
          </div>
        )}
        {isMe && (
          <Link to="/account/security" className="self-start text-[13px] font-semibold text-pink-ink hover:underline">
            {t("people.security.yours")}
          </Link>
        )}
      </div>
      <Dialog
        open={asking}
        onClose={() => setAsking(false)}
        title={t("people.security.resetTitle", { login: person.login })}
        width="sm"
      >
        <div className="flex flex-col gap-4 px-6 pt-1 pb-6">
          <p className="text-sm text-muted">{t("people.security.resetBody")}</p>
          {reset.isError && (
            <p role="alert" className="text-[13px] text-danger">
              {errorText(reset.error)}
            </p>
          )}
          <div className="flex justify-end gap-2">
            <Button onClick={() => setAsking(false)}>{t("common.cancel")}</Button>
            <Button
              variant="danger"
              icon={ShieldOff}
              busy={reset.isPending}
              onClick={() =>
                reset.mutate(undefined, {
                  onSuccess: () => {
                    toast(t("people.toasts.secondFactorsReset", { login: person.login }), "success");
                    setAsking(false);
                  },
                })
              }
            >
              {t("people.security.reset")}
            </Button>
          </div>
        </div>
      </Dialog>
    </Card>
  );
}

function PurgeDialog({ person, open, onClose }: { person: Person; open: boolean; onClose: () => void }) {
  const { t } = useT();
  const errorText = useErrorText();
  const purge = usePurgePerson(person.login);
  const [typed, setTyped] = useState("");
  return (
    <Dialog open={open} onClose={onClose} title={t("people.purge.title", { login: person.login })} width="sm">
      <form
        className="flex flex-col gap-4 px-6 pt-1 pb-6"
        onSubmit={(event) => {
          event.preventDefault();
          purge.mutate(typed, {
            onSuccess: () => {
              toast(t("people.toasts.purged", { login: person.login }), "success");
              onClose();
              navigate("/admin/people", { replace: true });
            },
          });
        }}
      >
        <p className="text-sm text-muted">{t("people.purge.body")}</p>
        <Field label={t("people.purge.confirm")} error={purge.isError && errorText(purge.error)}>
          {(id) => (
            <TextInput
              id={id}
              autoComplete="off"
              autoCapitalize="none"
              spellCheck={false}
              placeholder={person.login}
              value={typed}
              onChange={(event) => setTyped(event.target.value)}
            />
          )}
        </Field>
        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button
            type="submit"
            variant="danger"
            icon={Trash2}
            busy={purge.isPending}
            disabled={typed.trim().toLowerCase() !== person.login}
          >
            {t("people.purge.submit")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

export function PersonPage({ login, session }: { login: string; session: Session }) {
  const { t, i18n } = useT();
  const query = usePerson(login);
  const [purging, setPurging] = useState(false);
  const update = useUpdatePerson(login, (changes) =>
    changes.disabled === undefined
      ? t("people.toasts.saved")
      : t(changes.disabled ? "people.toasts.disabled" : "people.toasts.enabled", { login }),
  );
  const trash = useTrashPerson(login, () => t("people.toasts.trashed", { login }));
  const restore = useRestorePerson(login, () => t("people.toasts.restored", { login }));

  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const person = query.data;
  const isMe = person.login === session.account.login;
  const deleted = person.status === "deleted";

  return (
    <div className="flex flex-col gap-5">
      <Link
        to="/admin/people"
        className="inline-flex items-center gap-1.5 self-start rounded-full text-[13px] font-semibold text-muted hover:text-ink"
      >
        <ArrowLeft className="size-4" aria-hidden />
        {t("people.detail.back")}
      </Link>

      <header className="flex flex-wrap items-center gap-4">
        <PersonAvatar person={person} size="lg" />
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-[22px] font-bold tracking-[-0.01em]">
            {person.name || person.login.split("@")[0]}
          </h1>
          <p className="truncate text-sm text-muted">{person.login}</p>
        </div>
        <div className="flex flex-wrap gap-1.5">
          <StatusPill status={person.status} />
          {person.role === "admin" && <AdminPill />}
        </div>
      </header>

      {person.status === "invited" && <Banner tone="pink">{t("people.detail.banner.invited")}</Banner>}
      {person.status === "disabled" && (
        <Banner
          tone="warning"
          action={
            <Button
              size="sm"
              icon={LockOpen}
              busy={update.isPending}
              onClick={() => update.mutate({ disabled: false })}
            >
              {t("people.detail.enable")}
            </Button>
          }
        >
          {t("people.detail.banner.disabled")}
        </Banner>
      )}
      {deleted && person.purgeAt && (
        <Banner
          tone="danger"
          action={
            <Button size="sm" icon={RotateCcw} busy={restore.isPending} onClick={() => restore.mutate(undefined)}>
              {t("people.detail.restore")}
            </Button>
          }
        >
          {t("people.detail.banner.deleted", { date: formatDate(person.purgeAt, i18n.language) })}
        </Banner>
      )}

      <div className="grid gap-5 md:grid-cols-2">
        <Card title={t("people.detail.general")}>
          {deleted ? (
            // Someone in the trash can only be restored or deleted, so their settings are read-only.
            <div className="flex flex-col gap-3">
              <KeyValue label={t("people.detail.name")} value={person.name || "—"} />
              <KeyValue label={t("people.detail.admin")} value={person.role === "admin" ? "✓" : "—"} />
              <StorageLine person={person} />
            </div>
          ) : (
            <div className="flex flex-col gap-4">
              <NameField key={person.name} person={person} />
              <Toggle
                checked={person.role === "admin"}
                onChange={(admin) => update.mutate({ admin })}
                label={t("people.detail.admin")}
                description={t("people.detail.adminHint")}
              />
              <Field label={t("people.detail.quota")}>
                {(id) => (
                  <QuotaSelect
                    id={id}
                    value={person.quotaBytes}
                    onChange={(quotaBytes) => update.mutate({ quotaBytes })}
                  />
                )}
              </Field>
              <StorageLine person={person} />
            </div>
          )}
        </Card>

        <Addresses person={person} editable={!deleted} />

        {!deleted && <Access person={person} />}

        {!deleted && <SecurityInfo person={person} isMe={isMe} />}

        {!isMe && (
          <Card title={t("people.detail.danger")}>
            <div className="flex flex-col gap-4">
              {!deleted && (
                <>
                  <div className="flex flex-col items-start gap-2 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
                    <p className="min-w-0 flex-1 text-[13px] text-muted">{t("people.detail.disableHint")}</p>
                    {person.status === "disabled" ? (
                      <Button icon={LockOpen} onClick={() => update.mutate({ disabled: false })}>
                        {t("people.detail.enable")}
                      </Button>
                    ) : (
                      <Button icon={Lock} onClick={() => update.mutate({ disabled: true })}>
                        {t("people.detail.disable")}
                      </Button>
                    )}
                  </div>
                  <div className="flex flex-col items-start gap-2 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
                    <p className="min-w-0 flex-1 text-[13px] text-muted">{t("people.detail.trashHint")}</p>
                    <Button
                      variant="danger"
                      icon={Trash2}
                      busy={trash.isPending}
                      onClick={() => trash.mutate(undefined)}
                    >
                      {t("people.detail.trash")}
                    </Button>
                  </div>
                </>
              )}
              <div className="flex justify-end">
                <button
                  type="button"
                  className="rounded-full px-2 py-1 text-[13px] font-semibold text-danger hover:underline"
                  onClick={() => setPurging(true)}
                >
                  {t("people.detail.purge")}
                </button>
              </div>
            </div>
          </Card>
        )}
      </div>
      <PurgeDialog key={String(purging)} person={person} open={purging} onClose={() => setPurging(false)} />
    </div>
  );
}
