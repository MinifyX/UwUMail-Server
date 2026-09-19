import { History, LogOut, MonitorSmartphone } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { Field, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type SecurityView, type Session } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime, formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";
import { AppPasswordsCard } from "./AppPasswordsCard";
import { usePasswordConfirmation } from "./ConfirmPassword";
import { useSecurity, useSecurityAction } from "./queries";
import { describeDevice, useEventText } from "./SecurityBits";
import { TwoFactorCard } from "./TwoFactorCard";

const MIN_CHARS = 10;

function PasswordCard({ login }: { login: string }) {
  const { t } = useT();
  const errorText = useErrorText();
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [repeat, setRepeat] = useState("");
  const change = useSecurityAction(() =>
    api<void>("/api/account/password", { method: "POST", body: { current, new: next } }),
  );
  const missing = Math.max(0, MIN_CHARS - [...next].length);
  const mismatch = repeat.length > 0 && repeat !== next;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (missing > 0 || mismatch) return;
    change.mutate(undefined, {
      onSuccess: () => {
        toast(t("security.password.changed"), "success");
        setCurrent("");
        setNext("");
        setRepeat("");
      },
    });
  };
  return (
    <Card title={t("security.password.title")}>
      <form className="flex flex-col gap-3" onSubmit={submit}>
        <input type="email" autoComplete="username" value={login} readOnly hidden />
        <Field label={t("security.password.current")}>
          {(id) => (
            <TextInput
              id={id}
              type="password"
              autoComplete="current-password"
              required
              value={current}
              onChange={(event) => setCurrent(event.target.value)}
            />
          )}
        </Field>
        <Field
          label={t("security.password.new")}
          hint={next && missing > 0 ? t("password.missing", { count: missing }) : t("security.password.hint")}
        >
          {(id) => (
            <TextInput
              id={id}
              type="password"
              autoComplete="new-password"
              required
              value={next}
              onChange={(event) => setNext(event.target.value)}
            />
          )}
        </Field>
        <Field
          label={t("security.password.repeat")}
          error={mismatch ? t("password.mismatch") : change.isError ? errorText(change.error) : undefined}
        >
          {(id) => (
            <TextInput
              id={id}
              type="password"
              autoComplete="new-password"
              required
              value={repeat}
              onChange={(event) => setRepeat(event.target.value)}
            />
          )}
        </Field>
        <div className="flex justify-end">
          <Button type="submit" variant="primary" busy={change.isPending} disabled={missing > 0 || mismatch}>
            {t("security.password.submit")}
          </Button>
        </div>
      </form>
    </Card>
  );
}

function SessionsCard({ security }: { security: SecurityView }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const end = useSecurityAction((id: string) => api<void>(`/api/account/sessions/${id}`, { method: "DELETE" }));
  const endOthers = useSecurityAction(() =>
    api<{ ended: number }>("/api/account/sessions/end-others", { method: "POST", body: {} }),
  );
  const others = security.sessions.filter((session) => !session.current).length;
  return (
    <Card
      title={t("security.sessions.title")}
      action={
        others > 0 && (
          <Button
            size="sm"
            icon={LogOut}
            busy={endOthers.isPending}
            onClick={() =>
              endOthers.mutate(undefined, {
                onSuccess: (result) => toast(t("security.sessions.endedOthers", { count: result.ended }), "success"),
                onError: (error) => toast(errorText(error), "error"),
              })
            }
          >
            {t("security.sessions.endOthers")}
          </Button>
        )
      }
    >
      <ul className="flex flex-col">
        {security.sessions.map((session) => (
          <li key={session.id} className="flex items-center gap-3 border-b border-hairline py-2.5 last:border-b-0">
            <MonitorSmartphone className="size-4 shrink-0 text-muted" aria-hidden />
            <span className="min-w-0 flex-1">
              <span className="flex flex-wrap items-center gap-1.5 text-sm font-semibold">
                {describeDevice(session.userAgent, t)}
                {session.current && (
                  <span className="rounded-full bg-success-tint px-2 text-[11px] leading-5 font-semibold text-success">
                    {t("security.sessions.thisDevice")}
                  </span>
                )}
              </span>
              <span className="block text-[12px] text-muted">
                {t("security.sessions.seen", {
                  time: formatRelative(session.lastSeenAt, i18n.language),
                  ip: session.ip,
                })}
              </span>
            </span>
            {!session.current && (
              <Button
                size="sm"
                busy={end.isPending && end.variables === session.id}
                onClick={() =>
                  end.mutate(session.id, {
                    onSuccess: () => toast(t("security.sessions.ended"), "success"),
                    onError: (error) => toast(errorText(error), "error"),
                  })
                }
              >
                {t("security.sessions.end")}
              </Button>
            )}
          </li>
        ))}
      </ul>
    </Card>
  );
}

function ActivityCard({ security }: { security: SecurityView }) {
  const { t, i18n } = useT();
  const text = useEventText();
  const [all, setAll] = useState(false);
  const events = all ? security.events : security.events.slice(0, 8);
  return (
    <Card title={t("security.activity.title")}>
      {security.events.length === 0 ? (
        <p className="text-sm text-muted">{t("security.activity.none")}</p>
      ) : (
        <>
          <ul className="flex flex-col">
            {events.map((event) => (
              <li key={event.id} className="flex items-start gap-3 border-b border-hairline py-2.5 last:border-b-0">
                <History className="mt-0.5 size-4 shrink-0 text-muted" aria-hidden />
                <span className="min-w-0 flex-1">
                  <span className="block text-sm">{text(event)}</span>
                  <span className="block text-[12px] text-muted">
                    {formatDateTime(event.at, i18n.language)}
                    {event.ip && ` · ${event.ip}`}
                  </span>
                </span>
              </li>
            ))}
          </ul>
          {security.events.length > events.length && (
            <button
              type="button"
              className="mt-2 rounded-full text-[13px] font-semibold text-pink-ink hover:underline"
              onClick={() => setAll(true)}
            >
              {t("security.activity.more", { count: security.events.length - events.length })}
            </button>
          )}
        </>
      )}
    </Card>
  );
}

export function SecurityPage({ session }: { session: Session }) {
  const { t } = useT();
  const security = useSecurity();
  const { confirmed, dialog } = usePasswordConfirmation();

  if (security.isPending) return <Loading />;
  if (security.isError) return <LoadError error={security.error} onRetry={() => void security.refetch()} />;
  const data = security.data;

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("security.title")} intro={t("security.intro")} />
      {!data.secondFactor && session.account.role === "admin" && (
        <p className="rounded-card bg-warning-tint px-4 py-3 text-sm text-warning">{t("security.adminReminder")}</p>
      )}
      <div className="grid gap-5 lg:grid-cols-2">
        <div className="flex flex-col gap-5">
          <TwoFactorCard security={data} session={session} confirmed={confirmed} />
          <PasswordCard login={session.account.login} />
        </div>
        <div className="flex flex-col gap-5">
          <AppPasswordsCard security={data} session={session} confirmed={confirmed} />
          <SessionsCard security={data} />
          <ActivityCard security={data} />
        </div>
      </div>
      {dialog}
    </div>
  );
}
