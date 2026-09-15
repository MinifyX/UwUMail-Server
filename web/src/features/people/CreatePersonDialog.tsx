import { Check, Copy, X } from "lucide-react";
import { useState, type FormEvent } from "react";
import { NyuScene } from "@/components/nyu/scenes";
import { Button, IconButton } from "@/components/ui/Button";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Select, TextInput, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { absoluteUrl, type PasswordLinkCreated, type Person } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { navigate } from "@/lib/router";
import { usePrefs } from "@/state/prefs";
import { toast } from "@/state/toasts";
import { useCreatePerson, useDomains } from "./queries";

const GB = 1024 ** 3;
export const QUOTA_CHOICES = [0, 1, 2, 5, 10, 25, 50].map((gigabytes) => gigabytes * GB);

export function QuotaSelect({ id, value, onChange }: { id: string; value: number; onChange: (bytes: number) => void }) {
  const { t } = useT();
  const choices = QUOTA_CHOICES.includes(value) ? QUOTA_CHOICES : [...QUOTA_CHOICES, value].sort((a, b) => a - b);
  return (
    <Select id={id} value={String(value)} onChange={(event) => onChange(Number(event.target.value))}>
      {choices.map((bytes) => (
        <option key={bytes} value={bytes}>
          {bytes === 0
            ? t("people.create.unlimited")
            : t("people.create.gigabytes", { count: Math.round((bytes / GB) * 10) / 10 })}
        </option>
      ))}
    </Select>
  );
}

/** Shows an invitation or reset link with a copy button. */
export function LinkBox({ link }: { link: PasswordLinkCreated }) {
  const { t, i18n } = useT();
  const [copied, setCopied] = useState(false);
  const url = absoluteUrl(link.path);
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2 rounded-control border border-line bg-canvas p-1.5 pl-3">
        <code className="min-w-0 flex-1 truncate text-[13px] select-all">{url}</code>
        <Button
          size="sm"
          variant="primary"
          icon={copied ? Check : Copy}
          onClick={() => {
            void navigator.clipboard?.writeText(url).then(() => {
              setCopied(true);
              toast(t("people.toasts.linkCopied"), "success");
            });
          }}
        >
          {t("people.invite.copy")}
        </Button>
      </div>
      <p className="text-[12px] text-muted">
        {t("people.detail.linkValid", { date: formatDateTime(link.expiresAt, i18n.language) })}
      </p>
    </div>
  );
}

export function CreatePersonDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  // The dialog only renders its content while open, so every opening starts with an empty form.
  return (
    <Dialog open={open} onClose={onClose} width="sm">
      <CreatePerson onClose={onClose} />
    </Dialog>
  );
}

function CreatePerson({ onClose }: { onClose: () => void }) {
  const { t } = useT();
  const pro = usePrefs((s) => s.mode) === "pro";
  const domains = useDomains();
  const create = useCreatePerson();
  const errorText = useErrorText();

  const [name, setName] = useState("");
  const [localPart, setLocalPart] = useState("");
  const [domain, setDomain] = useState("");
  const [admin, setAdmin] = useState(false);
  const [quota, setQuota] = useState(0);
  const [ownPassword, setOwnPassword] = useState(false);
  const [password, setPassword] = useState("");
  const [created, setCreated] = useState<{ person: Person; link: PasswordLinkCreated | null } | null>(null);

  const chosenDomain = domain || domains.data?.[0]?.name || "";

  const submit = (event: FormEvent) => {
    event.preventDefault();
    create.mutate(
      {
        address: `${localPart.trim()}@${chosenDomain}`,
        name: name.trim(),
        admin,
        quotaBytes: quota,
        password: pro && ownPassword ? password : undefined,
      },
      {
        onSuccess: (result) => {
          if (result.link) {
            setCreated(result);
          } else {
            onClose();
            navigate(`/admin/people/${encodeURIComponent(result.person.login)}`);
          }
        },
      },
    );
  };

  const personName = created ? created.person.name || created.person.login : "";

  return (
    <>
      {created?.link ? (
        <div className="flex flex-col items-center gap-3 px-6 pt-6 pb-6 text-center">
          <NyuScene name="done" className="h-auto w-[180px]" />
          <h2 className="text-lg font-bold">{t("people.invite.title", { name: personName })}</h2>
          <p className="text-[13px] text-muted">{t("people.invite.body", { name: personName })}</p>
          <div className="w-full text-left">
            <LinkBox link={created.link} />
          </div>
          <div className="mt-2 flex w-full justify-end gap-2">
            <Button
              onClick={() => {
                onClose();
                navigate(`/admin/people/${encodeURIComponent(created.person.login)}`);
              }}
            >
              {t("people.invite.open")}
            </Button>
            <Button variant="primary" onClick={onClose}>
              {t("common.done")}
            </Button>
          </div>
        </div>
      ) : (
        <form className="flex flex-col gap-4 px-6 pt-5 pb-6" onSubmit={submit}>
          <header className="flex items-center justify-between gap-4">
            <h2 className="text-lg font-bold">{t("people.create.title")}</h2>
            <IconButton icon={X} label={t("common.close")} onClick={onClose} />
          </header>
          {domains.data?.length === 0 && (
            <p className="rounded-control bg-warning-tint px-3 py-2.5 text-[13px] text-warning">
              {t("people.create.noDomains")}
            </p>
          )}
          <Field label={t("people.create.name")}>
            {(id) => (
              <TextInput
                id={id}
                autoFocus
                placeholder={t("people.create.namePlaceholder")}
                value={name}
                onChange={(event) => setName(event.target.value)}
              />
            )}
          </Field>
          <Field label={t("people.create.address")}>
            {(id) => (
              <div className="flex items-center gap-2">
                <TextInput
                  id={id}
                  className="min-w-0 flex-1"
                  required
                  aria-label={t("people.create.localPart")}
                  autoComplete="off"
                  autoCapitalize="none"
                  spellCheck={false}
                  value={localPart}
                  onChange={(event) => setLocalPart(event.target.value.replace(/[@\s]/g, ""))}
                />
                <span className="text-muted">@</span>
                {(domains.data?.length ?? 0) > 1 ? (
                  <Select
                    aria-label={t("people.create.domain")}
                    className="min-w-0 flex-1"
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
                  <span className="shrink-0 text-sm font-semibold">{chosenDomain}</span>
                )}
              </div>
            )}
          </Field>
          <Field label={t("people.create.quota")}>
            {(id) => <QuotaSelect id={id} value={quota} onChange={setQuota} />}
          </Field>
          <Toggle
            checked={admin}
            onChange={setAdmin}
            label={t("people.create.admin")}
            description={t("people.create.adminHint")}
          />
          {pro && (
            <>
              <Toggle
                checked={ownPassword}
                onChange={setOwnPassword}
                label={t("people.create.ownPassword")}
                description={t("people.create.ownPasswordHint")}
              />
              {ownPassword && (
                <Field label={t("people.create.password")}>
                  {(id) => (
                    <TextInput
                      id={id}
                      type="password"
                      autoComplete="new-password"
                      required
                      minLength={10}
                      value={password}
                      onChange={(event) => setPassword(event.target.value)}
                    />
                  )}
                </Field>
              )}
            </>
          )}
          {create.isError && (
            <p role="alert" className="text-[13px] text-danger">
              {errorText(create.error)}
            </p>
          )}
          <div className="mt-1 flex justify-end gap-2">
            <Button onClick={onClose}>{t("common.cancel")}</Button>
            <Button type="submit" variant="primary" busy={create.isPending} disabled={!chosenDomain}>
              {t("people.create.submit")}
            </Button>
          </div>
        </form>
      )}
    </>
  );
}
