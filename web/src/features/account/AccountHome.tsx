import { useQuery } from "@tanstack/react-query";
import { NyuScene } from "@/components/nyu/scenes";
import { Card, CopyButton, KeyValue, PageHeader } from "@/components/ui/Card";
import { LoadError, Loading } from "@/components/StatusViews";
import { useT } from "@/i18n";
import { api, type Profile, type Session } from "@/lib/api";
import { formatBytes, formatDate } from "@/lib/format";
import { usePrefs } from "@/state/prefs";

function StorageBar({ used, quota }: { used: number; quota: number }) {
  const share = quota > 0 ? Math.min(1, used / quota) : 0;
  return (
    <div className="h-2.5 overflow-hidden rounded-full bg-canvas" aria-hidden>
      <div
        className={
          share > 0.9
            ? "h-full rounded-full bg-danger"
            : share > 0.75
              ? "h-full rounded-full bg-warning"
              : "h-full rounded-full bg-pink"
        }
        style={{ width: `${Math.max(share * 100, quota > 0 ? 2 : 0)}%` }}
      />
    </div>
  );
}

export function AccountHome({ session }: { session: Session }) {
  const { t, i18n } = useT();
  const mode = usePrefs((s) => s.mode);
  const profile = useQuery({ queryKey: ["account"], queryFn: () => api<Profile>("/api/account") });
  const hostname = session.server.hostname;
  const name = session.account.name || session.account.login.split("@")[0];

  if (profile.isPending) return <Loading />;
  if (profile.isError) return <LoadError error={profile.error} onRetry={() => void profile.refetch()} />;
  const data = profile.data;
  const language = i18n.language;
  const simple = mode === "simple";

  return (
    <div className="flex flex-col gap-5">
      <PageHeader
        title={t("account.greeting", { name })}
        intro={t("account.intro")}
        art={simple && <NyuScene name="welcome" className="h-auto w-[150px]" />}
      />

      <div className="grid gap-5 md:grid-cols-2">
        <Card title={t("account.addresses.title")}>
          <ul className="flex flex-col">
            {data.addresses.map((address) => (
              <li
                key={address}
                className="flex min-h-11 items-center justify-between gap-3 border-b border-hairline last:border-b-0"
              >
                <span className="min-w-0">
                  <span className="block truncate text-sm font-semibold">{address}</span>
                  {simple && (
                    <span className="block text-[12px] text-muted">
                      {address === data.login ? t("account.addresses.primary") : t("account.addresses.alias")}
                    </span>
                  )}
                </span>
                <CopyButton value={address} />
              </li>
            ))}
          </ul>
        </Card>

        <Card title={t("account.storage.title")}>
          <div className="flex flex-col gap-3">
            <StorageBar used={data.usedBytes} quota={data.quotaBytes} />
            <p className="text-sm text-muted">
              {data.quotaBytes > 0
                ? t("account.storage.usedOf", {
                    used: formatBytes(data.usedBytes, language),
                    quota: formatBytes(data.quotaBytes, language),
                  })
                : t("account.storage.used", { used: formatBytes(data.usedBytes, language) })}
            </p>
          </div>
        </Card>

        <Card title={t("account.apps.title")} className="md:col-span-2">
          {simple && <p className="mb-2 text-[13px] text-muted">{t("account.apps.intro")}</p>}
          <KeyValue label={t("account.apps.server")} value={hostname} copy={hostname} />
          <KeyValue label={t("account.apps.jmap")} value={`https://${hostname}`} copy={`https://${hostname}`} />
          <KeyValue label={t("account.apps.imap")} value={t("account.apps.imapValue", { hostname })} />
          <KeyValue label={t("account.apps.submission")} value={t("account.apps.submissionValue", { hostname })} />
          <KeyValue label={t("account.apps.username")} value={data.login} copy={data.login} />
        </Card>

        {!simple && (
          <Card title={t("account.details.title")} className="md:col-span-2">
            <KeyValue label={t("account.details.login")} value={data.login} />
            <KeyValue label={t("account.details.role")} value={t(`userMenu.role.${data.role}`)} />
            <KeyValue label={t("account.details.created")} value={formatDate(data.createdAt, language)} />
          </Card>
        )}
      </div>
    </div>
  );
}
