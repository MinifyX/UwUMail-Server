import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { BookUser, CalendarDays, LogOut, RefreshCw, Rss, Share2, Trash2, Unlink, UserMinus } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { Field, Select, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import {
  api,
  type CalendarSubscription,
  type CalendarsView,
  type OwnCollection,
  type ShareRights,
  type SharedCollection,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";
import { calendarsKey, ImportCard, INTERVALS } from "./ImportDialogs";

const RIGHTS: ShareRights[] = ["read", "write", "all"];

function KindIcon({ kind }: { kind: OwnCollection["kind"] }) {
  const Icon = kind === "calendar" ? CalendarDays : BookUser;
  return <Icon className="size-4 shrink-0 text-muted" aria-hidden />;
}

function Swatch({ color }: { color: string | null }) {
  if (!color) return null;
  return (
    <span
      className="size-3 shrink-0 rounded-full border border-hairline"
      style={{ backgroundColor: color.slice(0, 7) }}
      aria-hidden
    />
  );
}

/** Where a subscribed calendar comes from, how its last fetch went, and what can be done with it. */
function SubscriptionInfo({
  collection,
  subscription,
}: {
  collection: OwnCollection;
  subscription: CalendarSubscription;
}) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [ending, setEnding] = useState(false);
  const base = `/api/account/calendar-subscriptions/${subscription.id}`;
  const onError = (error: unknown) => toast(errorText(error), "error");
  const refresh = useMutation({
    mutationFn: () => api<CalendarsView>(`${base}/refresh`, { method: "POST" }),
    onSuccess: (next) => queryClient.setQueryData(calendarsKey, next),
    onError,
  });
  const change = useMutation({
    mutationFn: (body: { intervalSecs?: number; enabled?: boolean }) =>
      api<CalendarsView>(base, { method: "PATCH", body }),
    onSuccess: (next) => queryClient.setQueryData(calendarsKey, next),
    onError,
  });
  const end = useMutation({
    mutationFn: (keep: boolean) => api<CalendarsView>(`${base}?keep=${keep}`, { method: "DELETE" }),
    onSuccess: (next) => {
      queryClient.setQueryData(calendarsKey, next);
      toast(t("calendars.subscribe.endedToast", { name: collection.name }), "success");
    },
    onError,
  });
  const intervals: number[] = INTERVALS.includes(subscription.intervalSecs as (typeof INTERVALS)[number])
    ? [...INTERVALS]
    : [...INTERVALS, subscription.intervalSecs].sort((a, b) => a - b);

  return (
    <div className="flex flex-col gap-2 rounded-control bg-canvas p-3">
      <p className="flex items-center gap-2 text-sm">
        <Rss className="size-4 shrink-0 text-muted" aria-hidden />
        <span className="min-w-0 flex-1 truncate">
          {t("calendars.subscribe.from", { source: subscription.source })}
        </span>
      </p>
      <p className="text-[13px] text-muted">{t("calendars.subscribe.readOnly")}</p>
      {subscription.lastError ? (
        <p className="text-[13px] text-danger">
          {t("calendars.subscribe.failed", {
            error: t(`errors.codes.${subscription.lastError}`, t("errors.codes.internal")),
          })}
        </p>
      ) : (
        subscription.lastOkAt && (
          <p className="text-[13px] text-muted">
            {t("calendars.subscribe.updated", { time: formatRelative(subscription.lastOkAt, i18n.language) })}
          </p>
        )
      )}
      <div className="flex flex-wrap items-center gap-2">
        <Select
          aria-label={t("calendars.subscribe.interval")}
          className="w-auto"
          value={subscription.intervalSecs}
          onChange={(event) => change.mutate({ intervalSecs: Number(event.target.value) })}
        >
          {intervals.map((secs) => (
            <option key={secs} value={secs}>
              {t(`calendars.subscribe.every.${secs}`, t("calendars.subscribe.everyOther"))}
            </option>
          ))}
        </Select>
        <Button size="sm" icon={RefreshCw} busy={refresh.isPending} onClick={() => refresh.mutate()}>
          {t("calendars.subscribe.refresh")}
        </Button>
        {ending ? (
          <>
            <Button size="sm" icon={Unlink} busy={end.isPending && end.variables} onClick={() => end.mutate(true)}>
              {t("calendars.subscribe.endKeep")}
            </Button>
            <Button
              size="sm"
              variant="danger"
              icon={Trash2}
              busy={end.isPending && !end.variables}
              onClick={() => end.mutate(false)}
            >
              {t("calendars.subscribe.endDelete")}
            </Button>
          </>
        ) : (
          <Button size="sm" variant="danger" icon={Unlink} onClick={() => setEnding(true)}>
            {t("calendars.subscribe.end")}
          </Button>
        )}
      </div>
    </div>
  );
}

/** One of one's own calendars or address books, with who sees it and a form to share it further. */
function OwnCard({ collection }: { collection: OwnCollection }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [address, setAddress] = useState("");
  const [rights, setRights] = useState<ShareRights>("read");
  const share = useMutation({
    mutationFn: (request: { address: string; rights: ShareRights }) =>
      api<CalendarsView>(`/api/account/calendars/${collection.id}/shares`, { method: "PUT", body: request }),
    onSuccess: (next, request) => {
      queryClient.setQueryData(calendarsKey, next);
      setAddress("");
      toast(t("calendars.sharedToast", { name: collection.name, address: request.address.trim() }), "success");
    },
  });
  const unshare = useMutation({
    mutationFn: (accountId: number) =>
      api<CalendarsView>(`/api/account/calendars/${collection.id}/shares/${accountId}`, { method: "DELETE" }),
    onSuccess: (next) => {
      queryClient.setQueryData(calendarsKey, next);
      toast(t("calendars.unsharedToast", { name: collection.name }), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const changeRights = useMutation({
    mutationFn: (request: { address: string; rights: ShareRights }) =>
      api<CalendarsView>(`/api/account/calendars/${collection.id}/shares`, { method: "PUT", body: request }),
    onSuccess: (next) => queryClient.setQueryData(calendarsKey, next),
    onError: (error) => toast(errorText(error), "error"),
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    share.mutate({ address: address.trim(), rights });
  };

  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <KindIcon kind={collection.kind} />
          <Swatch color={collection.color} />
          <span className="truncate">{collection.name}</span>
        </span>
      }
    >
      <div className="flex flex-col gap-4">
        {collection.subscription && <SubscriptionInfo collection={collection} subscription={collection.subscription} />}
        {collection.shares.length === 0 ? (
          <p className="text-sm text-muted">{t("calendars.notShared")}</p>
        ) : (
          <ul className="flex flex-col" aria-label={t("calendars.sharedWith")}>
            {collection.shares.map((person) => (
              <li
                key={person.accountId}
                className="flex min-h-11 flex-wrap items-center gap-2 border-b border-hairline py-1.5 last:border-b-0"
              >
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-semibold">{person.name || person.address}</span>
                  {person.name && <span className="block truncate text-[12px] text-muted">{person.address}</span>}
                </span>
                <Select
                  aria-label={t("calendars.rightsFor", { address: person.address })}
                  className="w-auto"
                  value={person.rights}
                  onChange={(event) =>
                    changeRights.mutate({ address: person.address, rights: event.target.value as ShareRights })
                  }
                >
                  {RIGHTS.map((level) => (
                    <option key={level} value={level}>
                      {t(`calendars.rights.${level}`)}
                    </option>
                  ))}
                </Select>
                <Button
                  size="sm"
                  variant="danger"
                  icon={UserMinus}
                  busy={unshare.isPending && unshare.variables === person.accountId}
                  onClick={() => unshare.mutate(person.accountId)}
                >
                  {t("calendars.unshare")}
                </Button>
              </li>
            ))}
          </ul>
        )}

        <form className="flex flex-col gap-2" onSubmit={submit}>
          <Field
            label={t("calendars.shareWith")}
            hint={t(`calendars.rightsHint.${rights}`)}
            error={share.isError ? errorText(share.error) : undefined}
          >
            {(id) => (
              <div className="flex flex-wrap items-center gap-2">
                <TextInput
                  id={id}
                  type="email"
                  required
                  autoComplete="off"
                  autoCapitalize="none"
                  spellCheck={false}
                  placeholder={t("calendars.addressPlaceholder")}
                  className="min-w-0 flex-1 basis-48"
                  value={address}
                  onChange={(event) => setAddress(event.target.value)}
                />
                <Select
                  aria-label={t("calendars.rightsLabel")}
                  className="w-auto"
                  value={rights}
                  onChange={(event) => setRights(event.target.value as ShareRights)}
                >
                  {RIGHTS.map((level) => (
                    <option key={level} value={level}>
                      {t(`calendars.rights.${level}`)}
                    </option>
                  ))}
                </Select>
              </div>
            )}
          </Field>
          <div>
            <Button type="submit" icon={Share2} busy={share.isPending} disabled={!address.trim()}>
              {t("calendars.share")}
            </Button>
          </div>
        </form>
      </div>
    </Card>
  );
}

function SharedWithMe({ shared }: { shared: SharedCollection[] }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const leave = useMutation({
    mutationFn: (collection: SharedCollection) =>
      api<CalendarsView>(`/api/account/shared-calendars/${collection.id}`, { method: "DELETE" }),
    onSuccess: (next, collection) => {
      queryClient.setQueryData(calendarsKey, next);
      toast(t("calendars.leftToast", { name: collection.name }), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  return (
    <Card title={t("calendars.sharedTitle")} className="lg:col-span-2">
      {shared.length === 0 ? (
        <p className="text-sm text-muted">{t("calendars.sharedEmpty")}</p>
      ) : (
        <ul className="flex flex-col">
          {shared.map((collection) => (
            <li
              key={collection.id}
              className="flex min-h-11 flex-wrap items-center gap-2 border-b border-hairline py-1.5 last:border-b-0"
            >
              <KindIcon kind={collection.kind} />
              <Swatch color={collection.color} />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-sm font-semibold">{collection.name}</span>
                <span className="block truncate text-[12px] text-muted">
                  {t("calendars.from", { owner: collection.ownerName || collection.owner })} ·{" "}
                  {t(`calendars.rights.${collection.rights}`)}
                </span>
              </span>
              <Button
                size="sm"
                icon={LogOut}
                busy={leave.isPending && leave.variables?.id === collection.id}
                onClick={() => leave.mutate(collection)}
              >
                {t("calendars.leave")}
              </Button>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

export function CalendarsPage() {
  const { t } = useT();
  const calendars = useQuery({ queryKey: calendarsKey, queryFn: () => api<CalendarsView>("/api/account/calendars") });

  if (calendars.isPending) return <Loading />;
  if (calendars.isError) return <LoadError error={calendars.error} onRetry={() => void calendars.refetch()} />;
  const data = calendars.data;

  return (
    <div className="flex flex-col gap-5">
      <PageHeader
        title={
          <span className="flex items-center gap-2">
            <CalendarDays className="size-5 text-muted" aria-hidden />
            {t("calendars.pageTitle")}
          </span>
        }
        intro={t("calendars.intro")}
      />
      {!data.calendars && !data.contacts ? (
        <Card>
          <p className="text-sm text-muted">{t("calendars.off")}</p>
        </Card>
      ) : (
        <div className="grid gap-5 lg:grid-cols-2">
          <ImportCard view={data} />
          {data.own.map((collection) => (
            <OwnCard key={collection.id} collection={collection} />
          ))}
          <SharedWithMe shared={data.shared} />
        </div>
      )}
    </div>
  );
}
