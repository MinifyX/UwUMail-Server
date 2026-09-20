import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, Download, Pause, Play, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { ConfirmDiscardDialog } from "@/components/ui/ConfirmDiscardDialog";
import { Dialog } from "@/components/ui/Dialog";
import { EmptyState } from "@/components/ui/EmptyState";
import { Field, Select, TextInput, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type FetchAccountInfo, type FetchView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { toast } from "@/state/toasts";

const fetchKey = ["account", "fetch"] as const;

/** The IMAP server of a provider, guessed from the address so most people never have to type one. */
function guessHost(address: string): string {
  const domain = address.split("@")[1]?.trim().toLowerCase() ?? "";
  if (!domain.includes(".")) return "";
  // Apple keeps its mail under a different name than the address suggests.
  if (["icloud.com", "me.com", "mac.com"].includes(domain)) return "imap.mail.me.com";
  return `imap.${domain}`;
}

/** Where the provider takes outgoing mail, guessed the same way. */
function guessSendHost(address: string): string {
  const domain = address.split("@")[1]?.trim().toLowerCase() ?? "";
  if (!domain.includes(".")) return "";
  if (["icloud.com", "me.com", "mac.com"].includes(domain)) return "smtp.mail.me.com";
  return `smtp.${domain}`;
}

interface FormState {
  address: string;
  host: string;
  username: string;
  password: string;
  afterFetch: "markRead" | "delete";
  fetchJunk: boolean;
  intervalSecs: number;
  enabled: boolean;
  smtpHost: string;
  smtpPort: number;
  smtpSecurity: "starttls" | "tls";
  sendEnabled: boolean;
}

function emptyForm(defaultInterval: number): FormState {
  return {
    address: "",
    host: "",
    username: "",
    password: "",
    afterFetch: "markRead",
    fetchJunk: true,
    intervalSecs: defaultInterval,
    enabled: true,
    smtpHost: "",
    smtpPort: 587,
    smtpSecurity: "starttls",
    sendEnabled: false,
  };
}

function formOf(account: FetchAccountInfo): FormState {
  return {
    address: account.address,
    host: account.host,
    username: account.username,
    password: "",
    afterFetch: account.afterFetch,
    fetchJunk: account.fetchJunk,
    intervalSecs: account.intervalSecs,
    enabled: account.enabled,
    smtpHost: account.smtpHost,
    smtpPort: account.smtpPort,
    smtpSecurity: account.smtpSecurity,
    sendEnabled: account.sendEnabled,
  };
}

/** How a mailbox is doing, in one line. */
function Status({ account }: { account: FetchAccountInfo }) {
  const { t, i18n } = useT();
  if (!account.enabled) return <span className="text-[12px] text-muted">{t("fetch.status.paused")}</span>;
  if (account.lastError) {
    return (
      <span className="flex items-center gap-1.5 text-[12px] text-danger">
        <AlertTriangle className="size-3.5 shrink-0" aria-hidden />
        {account.lastError}
      </span>
    );
  }
  if (!account.lastOkAt) return <span className="text-[12px] text-muted">{t("fetch.status.waiting")}</span>;
  return (
    <span className="flex items-center gap-1.5 text-[12px] text-muted">
      <CheckCircle2 className="size-3.5 shrink-0 text-success" aria-hidden />
      {t("fetch.status.lastRun", { date: formatDateTime(account.lastOkAt, i18n.language) })}
    </span>
  );
}

function MailboxForm({
  view,
  account,
  onClose,
  onCancel,
  onDirtyChange,
}: {
  view: FetchView;
  account?: FetchAccountInfo;
  onClose: () => void;
  onCancel: () => void;
  onDirtyChange: (dirty: boolean) => void;
}) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [form, setForm] = useState<FormState>(() => (account ? formOf(account) : emptyForm(view.defaultIntervalSecs)));
  const [hostTouched, setHostTouched] = useState(Boolean(account));
  const [error, setError] = useState<string | null>(null);

  const change = <K extends keyof FormState>(key: K, value: FormState[K]) => {
    setForm((old) => ({ ...old, [key]: value }));
    onDirtyChange(true);
  };
  // Until someone types a server name themselves, it follows the address.
  const host = hostTouched ? form.host : guessHost(form.address);
  // The outgoing server usually sits under the same name as the incoming one.
  const sendHost = form.smtpHost || guessSendHost(form.address);

  const save = useMutation({
    mutationFn: async () => {
      const body = {
        address: form.address.trim(),
        host: host.trim(),
        username: form.username.trim() || form.address.trim(),
        afterFetch: form.afterFetch,
        fetchJunk: form.fetchJunk,
        intervalSecs: form.intervalSecs,
        enabled: form.enabled,
        sendEnabled: form.sendEnabled,
        ...(form.sendEnabled || form.smtpHost
          ? { smtpHost: sendHost.trim(), smtpPort: form.smtpPort, smtpSecurity: form.smtpSecurity }
          : {}),
        // An empty password on an existing mailbox means: keep the one that is stored.
        ...(form.password ? { password: form.password } : {}),
      };
      return account
        ? api<FetchAccountInfo>(`/api/account/fetch/${account.id}`, { method: "PATCH", body })
        : api<FetchAccountInfo>("/api/account/fetch", { method: "POST", body });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: fetchKey });
      onDirtyChange(false);
      toast(account ? t("fetch.form.saved") : t("fetch.form.added"), "success");
      onClose();
    },
    onError: (failure) => setError(errorText(failure)),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    if (!account && !form.password) {
      setError(t("fetch.form.needPassword"));
      return;
    }
    save.mutate();
  };

  return (
    <form className="flex flex-col gap-4" onSubmit={submit}>
      <h2 className="text-base font-semibold">{account ? t("fetch.form.editTitle") : t("fetch.form.addTitle")}</h2>
      <Field label={t("fetch.form.address")} hint={t("fetch.form.addressHint")}>
        {(id) => (
          <TextInput
            id={id}
            type="email"
            autoComplete="off"
            value={form.address}
            disabled={Boolean(account)}
            onChange={(event) => change("address", event.target.value)}
          />
        )}
      </Field>
      <Field label={t("fetch.form.host")} hint={t("fetch.form.hostHint")}>
        {(id) => (
          <TextInput
            id={id}
            value={host}
            onChange={(event) => {
              setHostTouched(true);
              change("host", event.target.value);
            }}
          />
        )}
      </Field>
      <Field label={t("fetch.form.username")} hint={t("fetch.form.usernameHint")}>
        {(id) => (
          <TextInput
            id={id}
            autoComplete="off"
            placeholder={form.address}
            value={form.username}
            onChange={(event) => change("username", event.target.value)}
          />
        )}
      </Field>
      <Field
        label={account ? t("fetch.form.newPassword") : t("fetch.form.password")}
        hint={account ? t("fetch.form.newPasswordHint") : t("fetch.form.passwordHint")}
      >
        {(id) => (
          <TextInput
            id={id}
            type="password"
            autoComplete="new-password"
            value={form.password}
            onChange={(event) => change("password", event.target.value)}
          />
        )}
      </Field>
      <Field label={t("fetch.form.afterFetch")} hint={t("fetch.form.afterFetchHint")}>
        {(id) => (
          <Select
            id={id}
            value={form.afterFetch}
            onChange={(event) => change("afterFetch", event.target.value as FormState["afterFetch"])}
          >
            <option value="markRead">{t("fetch.form.markRead")}</option>
            <option value="delete">{t("fetch.form.delete")}</option>
          </Select>
        )}
      </Field>
      <Field label={t("fetch.form.interval")}>
        {(id) => (
          <Select
            id={id}
            value={String(form.intervalSecs)}
            onChange={(event) => change("intervalSecs", Number(event.target.value))}
          >
            {[300, 900, 1800, 3600, 21600]
              .filter((seconds) => seconds >= view.minIntervalSecs && seconds <= view.maxIntervalSecs)
              .map((seconds) => (
                <option key={seconds} value={seconds}>
                  {seconds < 3600
                    ? t("fetch.form.everyMinutes", { count: seconds / 60 })
                    : t("fetch.form.everyHours", { count: seconds / 3600 })}
                </option>
              ))}
          </Select>
        )}
      </Field>
      <Toggle
        checked={form.fetchJunk}
        onChange={(value) => change("fetchJunk", value)}
        label={t("fetch.form.junk")}
        description={t("fetch.form.junkHint")}
      />
      <Toggle
        checked={form.sendEnabled}
        onChange={(value) => change("sendEnabled", value)}
        label={t("fetch.form.send")}
        description={t("fetch.form.sendHint")}
      />
      {form.sendEnabled && (
        <>
          <Field label={t("fetch.form.smtpHost")} hint={t("fetch.form.smtpHostHint")}>
            {(id) => (
              <TextInput id={id} value={sendHost} onChange={(event) => change("smtpHost", event.target.value)} />
            )}
          </Field>
          <Field label={t("fetch.form.smtpSecurity")}>
            {(id) => (
              <Select
                id={id}
                value={`${form.smtpSecurity}:${form.smtpPort}`}
                onChange={(event) => {
                  const [security, port] = event.target.value.split(":");
                  change("smtpSecurity", security as FormState["smtpSecurity"]);
                  change("smtpPort", Number(port));
                }}
              >
                <option value="starttls:587">{t("fetch.form.starttls")}</option>
                <option value="tls:465">{t("fetch.form.implicitTls")}</option>
              </Select>
            )}
          </Field>
        </>
      )}
      {error && <p className="text-[13px] text-danger">{error}</p>}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" onClick={onCancel} type="button">
          {t("common.cancel")}
        </Button>
        <Button variant="primary" type="submit" busy={save.isPending}>
          {account ? t("common.save") : t("fetch.form.add")}
        </Button>
      </div>
    </form>
  );
}

function MailboxDialog({
  open,
  view,
  account,
  onClose,
}: {
  open: boolean;
  view: FetchView;
  account?: FetchAccountInfo;
  onClose: () => void;
}) {
  const [dirty, setDirty] = useState(false);
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);

  // Closing forgets the unsaved input, so the next opening starts clean without an effect.
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
      <Dialog open={open} onClose={requestClose} closeOnOutsideClick={!dirty} width="sm">
        <MailboxForm view={view} account={account} onClose={close} onCancel={requestClose} onDirtyChange={setDirty} />
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

export function FetchPage() {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [editing, setEditing] = useState<FetchAccountInfo | null>(null);
  const [adding, setAdding] = useState(false);
  const view = useQuery({ queryKey: fetchKey, queryFn: () => api<FetchView>("/api/account/fetch") });

  const refresh = () => void queryClient.invalidateQueries({ queryKey: fetchKey });
  const runNow = useMutation({
    mutationFn: (id: number) => api<void>(`/api/account/fetch/${id}/run`, { method: "POST" }),
    onSuccess: () => toast(t("fetch.toasts.runQueued"), "success"),
    onError: (failure) => toast(errorText(failure), "error"),
  });
  const setEnabled = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) =>
      api<FetchAccountInfo>(`/api/account/fetch/${id}`, { method: "PATCH", body: { enabled } }),
    onSuccess: refresh,
    onError: (failure) => toast(errorText(failure), "error"),
  });
  const remove = useMutation({
    mutationFn: (id: number) => api<void>(`/api/account/fetch/${id}`, { method: "DELETE" }),
    onSuccess: () => {
      refresh();
      toast(t("fetch.toasts.removed"), "success");
    },
    onError: (failure) => toast(errorText(failure), "error"),
  });

  if (view.isPending) return <Loading />;
  if (view.isError) return <LoadError error={view.error} onRetry={() => void view.refetch()} />;

  const accounts = view.data.accounts;
  const full = accounts.length >= view.data.max;

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title={t("fetch.title")} intro={t("fetch.intro")} />
      <Card title={t("fetch.card.title")}>
        <div className="flex flex-col gap-4">
          {accounts.length === 0 ? (
            <EmptyState scene="inbox" title={t("fetch.empty.title")} body={t("fetch.empty.body")} />
          ) : (
            <ul className="flex flex-col">
              {accounts.map((account) => (
                <li
                  key={account.id}
                  className="flex flex-wrap items-center gap-3 border-b border-hairline py-3 last:border-b-0"
                >
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-sm font-semibold">{account.address}</span>
                    <Status account={account} />
                    <span className="block text-[12px] text-muted">
                      {t("fetch.card.brought", { count: account.totalFetched })}
                    </span>
                  </span>
                  <div className="flex items-center gap-1.5">
                    <IconButton
                      icon={RefreshCw}
                      label={t("fetch.actions.runNow")}
                      onClick={() => runNow.mutate(account.id)}
                    />
                    <IconButton
                      icon={account.enabled ? Pause : Play}
                      label={account.enabled ? t("fetch.actions.pause") : t("fetch.actions.resume")}
                      onClick={() => setEnabled.mutate({ id: account.id, enabled: !account.enabled })}
                    />
                    <Button size="sm" variant="ghost" onClick={() => setEditing(account)}>
                      {t("fetch.actions.edit")}
                    </Button>
                    <IconButton
                      icon={Trash2}
                      label={t("fetch.actions.remove")}
                      onClick={() => {
                        if (window.confirm(t("fetch.actions.removeConfirm", { address: account.address }))) {
                          remove.mutate(account.id);
                        }
                      }}
                    />
                  </div>
                </li>
              ))}
            </ul>
          )}
          <div className="flex items-center justify-between gap-3">
            <p className="text-[12px] text-muted">{t("fetch.card.hint")}</p>
            <Button variant="primary" icon={Plus} disabled={full} onClick={() => setAdding(true)}>
              {t("fetch.card.add")}
            </Button>
          </div>
        </div>
      </Card>
      <Card title={t("fetch.about.title")}>
        <div className="flex flex-col gap-2 text-[13px] text-muted">
          <p className="flex items-start gap-2">
            <Download className="mt-0.5 size-4 shrink-0" aria-hidden />
            {t("fetch.about.spam")}
          </p>
          <p>{t("fetch.about.first")}</p>
          <p>{t("fetch.about.password")}</p>
        </div>
      </Card>
      <MailboxDialog open={adding} view={view.data} onClose={() => setAdding(false)} />
      <MailboxDialog
        open={editing !== null}
        view={view.data}
        account={editing ?? undefined}
        onClose={() => setEditing(null)}
      />
    </div>
  );
}
