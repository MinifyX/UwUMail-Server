import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, Clock, Plus, TreePalm, Trash2 } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { NyuScene } from "@/components/nyu/scenes";
import { Button } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { Field, TextInput, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type ForwardingView, type VacationView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDate } from "@/lib/format";
import { usePrefs } from "@/state/prefs";
import { toast } from "@/state/toasts";

const forwardingKey = ["account", "forwarding"] as const;
const vacationKey = ["account", "vacation"] as const;

function ForwardingCard({ forwarding }: { forwarding: ForwardingView }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const simple = usePrefs((s) => s.mode) === "simple";
  const queryClient = useQueryClient();
  const [address, setAddress] = useState("");
  const saved = (data: ForwardingView) => queryClient.setQueryData(forwardingKey, data);
  const add = useMutation({
    mutationFn: (value: string) =>
      api<ForwardingView>("/api/account/forwarding/targets", { method: "POST", body: { address: value } }),
    onSuccess: (data, value) => {
      saved(data);
      setAddress("");
      const target = data.targets.find((entry) => entry.address === value.trim().toLowerCase());
      toast(
        target && !target.confirmedAt
          ? t("mailbox.forwarding.pendingToast", { address: target.address })
          : t("mailbox.forwarding.added"),
        "success",
      );
    },
  });
  const remove = useMutation({
    mutationFn: (id: number) => api<ForwardingView>(`/api/account/forwarding/targets/${id}`, { method: "DELETE" }),
    onSuccess: saved,
    onError: (error) => toast(errorText(error), "error"),
  });
  const keepCopy = useMutation({
    mutationFn: (keep: boolean) =>
      api<ForwardingView>("/api/account/forwarding/keep-copy", { method: "PUT", body: { keep } }),
    onSuccess: saved,
    onError: (error) => toast(errorText(error), "error"),
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    add.mutate(address);
  };
  const full = forwarding.targets.length >= forwarding.maxTargets;

  return (
    <Card title={t("mailbox.forwarding.title")}>
      <div className="flex flex-col gap-4">
        {simple && <p className="text-[13px] text-muted">{t("mailbox.forwarding.explain")}</p>}
        {!forwarding.externalAllowed && (
          <p className="rounded-control bg-canvas px-3 py-2.5 text-[13px] text-muted">
            {t("mailbox.forwarding.onlyLocal")}
          </p>
        )}
        {forwarding.targets.length > 0 && (
          <ul className="flex flex-col">
            {forwarding.targets.map((target) => (
              <li key={target.id} className="flex items-center gap-3 border-b border-hairline py-2.5 last:border-b-0">
                {target.confirmedAt ? (
                  <CheckCircle2 className="size-4 shrink-0 text-success" aria-hidden />
                ) : (
                  <Clock className="size-4 shrink-0 text-warning" aria-hidden />
                )}
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-semibold">{target.address}</span>
                  <span className="block text-[12px] text-muted">
                    {target.confirmedAt
                      ? target.local
                        ? t("mailbox.forwarding.activeLocal")
                        : t("mailbox.forwarding.active", { date: formatDate(target.confirmedAt, i18n.language) })
                      : t("mailbox.forwarding.pending")}
                  </span>
                </span>
                <Button
                  size="sm"
                  variant="danger"
                  icon={Trash2}
                  busy={remove.isPending && remove.variables === target.id}
                  onClick={() => remove.mutate(target.id)}
                >
                  {t("mailbox.forwarding.remove")}
                </Button>
              </li>
            ))}
          </ul>
        )}
        {full ? (
          <p className="text-[13px] text-muted">{t("mailbox.forwarding.full", { count: forwarding.maxTargets })}</p>
        ) : (
          <form className="flex flex-col gap-2 sm:flex-row sm:items-start" onSubmit={submit}>
            <Field
              label={t("mailbox.forwarding.address")}
              error={add.isError ? errorText(add.error) : undefined}
              className="min-w-0 flex-1"
            >
              {(id) => (
                <TextInput
                  id={id}
                  type="email"
                  required
                  autoComplete="off"
                  placeholder="name@example.org"
                  value={address}
                  onChange={(event) => setAddress(event.target.value)}
                />
              )}
            </Field>
            <Button type="submit" icon={Plus} busy={add.isPending} className="sm:mt-[26px]">
              {t("mailbox.forwarding.add")}
            </Button>
          </form>
        )}
        {forwarding.targets.length > 0 && (
          <Toggle
            checked={forwarding.keepCopy}
            onChange={(keep) => keepCopy.mutate(keep)}
            label={t("mailbox.forwarding.keepCopy")}
            description={t("mailbox.forwarding.keepCopyHint")}
          />
        )}
      </div>
    </Card>
  );
}

/** A date input value ("2026-09-20") from a Unix time, in the viewer's time zone. */
function dateValue(unix: number | null): string {
  if (!unix) return "";
  const date = new Date(unix * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

function unixFromDate(value: string, endOfDay: boolean): number | null {
  if (!value) return null;
  const [year, month, day] = value.split("-").map(Number);
  const date = endOfDay ? new Date(year!, month! - 1, day!, 23, 59, 59) : new Date(year!, month! - 1, day!, 0, 0, 0);
  return Math.floor(date.getTime() / 1000);
}

function VacationForm({ vacation }: { vacation: VacationView }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [enabled, setEnabled] = useState(vacation.isEnabled);
  const [from, setFrom] = useState(dateValue(vacation.fromDate));
  const [to, setTo] = useState(dateValue(vacation.toDate));
  const [subject, setSubject] = useState(vacation.subject ?? "");
  const [text, setText] = useState(vacation.textBody ?? "");
  const save = useMutation({
    mutationFn: () =>
      api<VacationView>("/api/account/vacation", {
        method: "PUT",
        body: {
          isEnabled: enabled,
          fromDate: unixFromDate(from, false),
          toDate: unixFromDate(to, true),
          subject,
          textBody: text,
        },
      }),
    onSuccess: (data) => {
      queryClient.setQueryData(vacationKey, data);
      toast(data.isEnabled ? t("mailbox.vacation.savedOn") : t("mailbox.vacation.savedOff"), "success");
    },
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    save.mutate();
  };

  return (
    <form className="flex flex-col gap-4" onSubmit={submit}>
      <Toggle
        checked={enabled}
        onChange={setEnabled}
        label={t("mailbox.vacation.enabled")}
        description={t("mailbox.vacation.enabledHint")}
      />
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label={t("mailbox.vacation.from")} hint={t("mailbox.vacation.optional")}>
          {(id) => <TextInput id={id} type="date" value={from} onChange={(event) => setFrom(event.target.value)} />}
        </Field>
        <Field label={t("mailbox.vacation.to")} hint={t("mailbox.vacation.optional")}>
          {(id) => <TextInput id={id} type="date" value={to} onChange={(event) => setTo(event.target.value)} />}
        </Field>
      </div>
      <Field label={t("mailbox.vacation.subject")}>
        {(id) => (
          <TextInput
            id={id}
            maxLength={200}
            placeholder={t("mailbox.vacation.subjectPlaceholder")}
            value={subject}
            onChange={(event) => setSubject(event.target.value)}
          />
        )}
      </Field>
      <Field label={t("mailbox.vacation.text")} error={save.isError ? errorText(save.error) : undefined}>
        {(id) => (
          <textarea
            id={id}
            rows={6}
            maxLength={10_000}
            placeholder={t("mailbox.vacation.textPlaceholder")}
            className="w-full rounded-control border border-line bg-surface px-3.5 py-2.5 text-sm focus:border-pink focus:shadow-focus focus:outline-none"
            value={text}
            onChange={(event) => setText(event.target.value)}
          />
        )}
      </Field>
      <div className="flex justify-end">
        <Button type="submit" variant="primary" busy={save.isPending}>
          {t("mailbox.vacation.save")}
        </Button>
      </div>
    </form>
  );
}

export function MailboxPage() {
  const { t } = useT();
  const simple = usePrefs((s) => s.mode) === "simple";
  const forwarding = useQuery({
    queryKey: forwardingKey,
    queryFn: () => api<ForwardingView>("/api/account/forwarding"),
  });
  const vacation = useQuery({ queryKey: vacationKey, queryFn: () => api<VacationView>("/api/account/vacation") });

  if (forwarding.isPending || vacation.isPending) return <Loading />;
  if (forwarding.isError) return <LoadError error={forwarding.error} onRetry={() => void forwarding.refetch()} />;
  if (vacation.isError) return <LoadError error={vacation.error} onRetry={() => void vacation.refetch()} />;

  return (
    <div className="flex flex-col gap-5">
      <PageHeader
        title={t("mailbox.title")}
        intro={t("mailbox.intro")}
        art={simple && <NyuScene name="inbox" className="h-auto w-[140px]" />}
      />
      <div className="grid gap-5 lg:grid-cols-2">
        <ForwardingCard forwarding={forwarding.data} />
        <Card
          title={
            <span className="flex items-center gap-2">
              <TreePalm className="size-4 text-muted" aria-hidden />
              {t("mailbox.vacation.title")}
            </span>
          }
        >
          <VacationForm key={JSON.stringify(vacation.data)} vacation={vacation.data} />
        </Card>
      </div>
    </div>
  );
}
