import { useQuery } from "@tanstack/react-query";
import { ChevronDown, ChevronUp } from "lucide-react";
import { useState } from "react";
import { Card, CopyButton, KeyValue, PageHeader } from "@/components/ui/Card";
import { LoadError, Loading } from "@/components/StatusViews";
import { useT } from "@/i18n";
import { api, type Profile, type Session } from "@/lib/api";
import { formatBytes, formatDate } from "@/lib/format";
import { AppleProfile } from "./AppleProfile";

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

/** How many addresses the card shows before somebody asks for the rest. */
const SHOWN_ADDRESSES = 3;

/**
 * Someone with two addresses wants to see both; someone with twenty wants to see the page. So the
 * card keeps its size and the rest is one click away, and the button says how many that is.
 */
function AddressList({ addresses }: { addresses: string[] }) {
  const { t } = useT();
  const [expanded, setExpanded] = useState(false);
  const hidden = addresses.length - SHOWN_ADDRESSES;
  const shown = expanded ? addresses : addresses.slice(0, SHOWN_ADDRESSES);

  return (
    <Card title={t("account.addresses.title")}>
      <ul className="flex flex-col">
        {shown.map((address) => (
          <li
            key={address}
            className="flex min-h-11 items-center justify-between gap-3 border-b border-hairline last:border-b-0"
          >
            <span className="min-w-0">
              <span className="block truncate text-sm font-semibold">{address}</span>
            </span>
            <CopyButton value={address} />
          </li>
        ))}
      </ul>
      {hidden > 0 && (
        <button
          type="button"
          className="mt-3 flex items-center gap-1 text-[13px] font-semibold text-pink hover:underline"
          onClick={() => setExpanded(!expanded)}
          aria-expanded={expanded}
        >
          {expanded ? <ChevronUp className="size-4" aria-hidden /> : <ChevronDown className="size-4" aria-hidden />}
          {expanded ? t("account.addresses.less") : t("account.addresses.more", { count: hidden })}
        </button>
      )}
    </Card>
  );
}

export function AccountHome({ session }: { session: Session }) {
  const { t, i18n } = useT();
  const profile = useQuery({ queryKey: ["account"], queryFn: () => api<Profile>("/api/account") });
  const hostname = session.server.hostname;
  const name = session.account.name || session.account.login.split("@")[0];

  if (profile.isPending) return <Loading />;
  if (profile.isError) return <LoadError error={profile.error} onRetry={() => void profile.refetch()} />;
  const data = profile.data;
  const language = i18n.language;

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("account.greeting", { name })} intro={t("account.intro")} />

      <div className="grid gap-5 md:grid-cols-2">
        <AddressList addresses={data.addresses} />

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
          <KeyValue label={t("account.apps.server")} value={hostname} copy={hostname} />
          <KeyValue label={t("account.apps.jmap")} value={`https://${hostname}`} copy={`https://${hostname}`} />
          <KeyValue label={t("account.apps.imap")} value={t("account.apps.imapValue", { hostname })} />
          <KeyValue label={t("account.apps.submission")} value={t("account.apps.submissionValue", { hostname })} />
          <KeyValue label={t("account.apps.username")} value={data.login} copy={data.login} />
          <AppleProfile />
        </Card>

        <Card title={t("account.details.title")} className="md:col-span-2">
          <KeyValue label={t("account.details.login")} value={data.login} />
          <KeyValue label={t("account.details.role")} value={t(`userMenu.role.${data.role}`)} />
          <KeyValue label={t("account.details.created")} value={formatDate(data.createdAt, language)} />
        </Card>
      </div>
    </div>
  );
}
