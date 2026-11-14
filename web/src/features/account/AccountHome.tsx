import {
  BUTTON_SIZES,
  BUTTON_VARIANTS,
  cx,
  Card,
  ICONS,
  Icon as SuiteIcon,
  ProgressBar,
  StatTile,
  StatusDot,
  motionAllowed,
} from "@uwusuite/design";
import { useQuery } from "@tanstack/react-query";
import type { LucideIcon } from "lucide-react";
import { useState, type ReactNode } from "react";
import { CopyButton, KeyValue, PageHeader } from "@/components/ui/Page";
import { LoadError, Loading } from "@/components/StatusViews";
import { useSecurity } from "@/features/security/queries";
import { useT } from "@/i18n";
import {
  api,
  type FetchView,
  type MaskedAddressesView,
  type Profile,
  type SecurityView,
  type Session,
} from "@/lib/api";
import { formatBytes, formatDate } from "@/lib/format";
import { Link } from "@/lib/router";
import { PictureCard } from "@/features/pictures/PictureCard";
import { AppleProfile } from "./AppleProfile";
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
          {expanded ? (
            <SuiteIcon icon={ICONS.collapse} className="size-4" aria-hidden />
          ) : (
            <SuiteIcon icon={ICONS.expand} className="size-4" aria-hidden />
          )}
          {expanded ? t("account.addresses.less") : t("account.addresses.more", { count: hidden })}
        </button>
      )}
    </Card>
  );
}

type Tone = "success" | "warning" | "danger";
const DOT_STATES = { success: "online", warning: "error", danger: "danger" } as const;

/** One line of the status card: a dot and what it means, so the colour is never the only hint. */
interface StatusLine {
  tone: Tone;
  text: string;
}

const storageShare = (profile: Profile) => (profile.quotaBytes > 0 ? profile.usedBytes / profile.quotaBytes : 0);

/**
 * What the overview can say about the account, from what the pages already load: storage, the
 * second factor and the fetched mail. A part that did not load says nothing rather than guessing.
 */
function useStatusLines(profile: Profile, security: SecurityView | undefined, fetch: FetchView | undefined) {
  const { t } = useT();
  const lines: StatusLine[] = [];
  const share = storageShare(profile);
  const percent = Math.round(share * 100);
  if (profile.quotaBytes > 0) {
    lines.push(
      share > 0.9
        ? { tone: "danger", text: t("account.status.storageFull", { percent }) }
        : share > 0.75
          ? { tone: "warning", text: t("account.status.storageFilling", { percent }) }
          : { tone: "success", text: t("account.status.storageOk", { percent }) },
    );
  }
  if (security) {
    lines.push(
      security.secondFactor
        ? { tone: "success", text: t("account.status.secondFactorOn") }
        : { tone: "warning", text: t("account.status.secondFactorOff") },
    );
  }
  const running = (fetch?.accounts ?? []).filter((account) => account.enabled);
  for (const account of running.filter((account) => account.lastError)) {
    lines.push({ tone: "danger", text: t("account.status.fetchFailed", { address: account.address }) });
  }
  const fine = running.filter((account) => !account.lastError).length;
  if (fine > 0) lines.push({ tone: "success", text: t("account.status.fetchOk", { count: fine }) });
  return lines;
}

function StatusCard({ lines }: { lines: StatusLine[] }) {
  const { t } = useT();
  const open = lines.filter((line) => line.tone !== "success").length;
  return (
    <Card
      title={t("account.status.title")}
      subtitle={open === 0 ? t("account.status.allGood") : t("account.status.open", { count: open })}
    >
      <ul className="flex flex-col gap-2.5">
        {lines.map((line) => (
          <li key={line.text} className="flex items-center gap-2.5 text-[13px]">
            <StatusDot state={DOT_STATES[line.tone]} />
            <span className="min-w-0">{line.text}</span>
          </li>
        ))}
      </ul>
    </Card>
  );
}

function NextRow({
  icon: Icon,
  title,
  text,
  to,
  onClick,
}: {
  icon: LucideIcon;
  title: ReactNode;
  text: ReactNode;
  to?: string;
  onClick?: () => void;
}) {
  const className =
    "flex min-h-[52px] w-full items-center gap-3 rounded-xl border border-hairline px-3 py-2 text-left hover:bg-elevated";
  const body = (
    <>
      <span className="flex size-8 shrink-0 items-center justify-center rounded-[10px] bg-pink-tint text-pink-ink">
        <Icon className="size-4" aria-hidden />
      </span>
      <span className="flex min-w-0 flex-1 flex-col leading-tight">
        <span className="text-sm font-semibold">{title}</span>
        <span className="text-[12px] text-muted">{text}</span>
      </span>
      <SuiteIcon icon={ICONS.next} className="size-4 shrink-0 text-muted" aria-hidden />
    </>
  );
  return to ? (
    <Link to={to} className={className}>
      {body}
    </Link>
  ) : (
    <button type="button" className={className} onClick={onClick}>
      {body}
    </button>
  );
}

/** Small things that make the account better; the second factor only while it is off. */
function NextCard({ security }: { security: SecurityView | undefined }) {
  const { t } = useT();
  return (
    <Card title={t("account.next.title")} subtitle={t("account.next.intro")}>
      <div className="flex flex-col gap-2">
        <NextRow
          icon={ICONS.secret}
          to="/account/codes"
          title={t("account.next.codes.title")}
          text={t("account.next.codes.text")}
        />
        {security && !security.secondFactor && (
          <NextRow
            icon={ICONS.securityCheck}
            to="/account/security"
            title={t("account.next.secondFactor.title")}
            text={t("account.next.secondFactor.text")}
          />
        )}
        <NextRow
          icon={ICONS.hide}
          to="/account/masked"
          title={t("account.next.masked.title")}
          text={t("account.next.masked.text")}
        />
        <NextRow
          icon={ICONS.devices}
          title={t("account.next.device.title")}
          text={t("account.next.device.text")}
          onClick={() =>
            document
              .getElementById("apps")
              ?.scrollIntoView({ behavior: motionAllowed() ? "smooth" : "auto", block: "start" })
          }
        />
      </div>
    </Card>
  );
}

export function AccountHome({ session }: { session: Session }) {
  const { t, i18n } = useT();
  const profile = useQuery({ queryKey: ["account"], queryFn: () => api<Profile>("/api/account") });
  // The same queries as the security, masked and fetch pages, so going there later is instant.
  const security = useSecurity();
  const masked = useQuery({
    queryKey: ["account", "masked"],
    queryFn: () => api<MaskedAddressesView>("/api/account/masked"),
  });
  const fetch = useQuery({ queryKey: ["account", "fetch"], queryFn: () => api<FetchView>("/api/account/fetch") });
  const hostname = session.server.hostname;
  const name = session.account.name || session.account.login.split("@")[0];
  const language = i18n.language;
  const lines = useStatusLines(profile.data ?? EMPTY_PROFILE, security.data, fetch.data);

  if (profile.isPending) return <Loading />;
  if (profile.isError) return <LoadError error={profile.error} onRetry={() => void profile.refetch()} />;
  const data = profile.data;
  const share = storageShare(data);
  const maskedActive = masked.data?.addresses.filter((address) => address.state === "enabled").length;
  const today = new Intl.DateTimeFormat(language, { weekday: "long", day: "numeric", month: "long" }).format(
    new Date(),
  );

  return (
    <div className="flex flex-col gap-5">
      <PageHeader
        large
        eyebrow={today}
        title={t("account.greeting", { name })}
        intro={t("account.intro")}
        actions={
          session.webmail && (
            <a
              href="/mail"
              className={cx(
                "inline-flex items-center justify-center rounded-full font-semibold focus-visible:shadow-focus",
                BUTTON_VARIANTS.primary,
                BUTTON_SIZES.lg,
              )}
            >
              <SuiteIcon icon={ICONS.inbox} className="size-4" strokeWidth={2.2} aria-hidden />
              {t("nav.openMailbox")}
            </a>
          )
        }
      />

      <div className="uwu-stagger grid grid-cols-2 gap-3 lg:grid-cols-4">
        <StatTile
          icon={ICONS.alias}
          value={data.addresses.length}
          format={(count) => t("account.tiles.addresses", { count })}
          label={
            maskedActive === undefined
              ? t("account.tiles.addressesNote")
              : t("account.tiles.masked", { count: maskedActive })
          }
        />
        <StatTile
          icon={ICONS.drive}
          tone={share > 0.9 ? "danger" : share > 0.75 ? "warning" : "normal"}
          value={data.usedBytes}
          format={(n) => formatBytes(n, language)}
          label={
            data.quotaBytes > 0
              ? t("account.tiles.storageOf", { quota: formatBytes(data.quotaBytes, language) })
              : t("account.tiles.storageUnlimited")
          }
        >
          {data.quotaBytes > 0 && <ProgressBar kind="usage" share={share} />}
        </StatTile>
        <StatTile
          icon={ICONS.securityCheck}
          tone={security.data && !security.data.secondFactor ? "warning" : "normal"}
          value={
            security.data
              ? security.data.secondFactor
                ? t("account.tiles.secondFactorOn")
                : t("account.tiles.secondFactorOff")
              : "–"
          }
          label={
            security.data
              ? t("account.tiles.passkeys", { count: security.data.passkeys.length })
              : t("account.tiles.secondFactor")
          }
        />
        <StatTile icon={ICONS.calendar} value={formatDate(data.createdAt, language)} label={t("account.tiles.since")} />
      </div>

      <div className="uwu-stagger grid gap-5 lg:grid-cols-[1.3fr_1fr]">
        <NextCard security={security.data} />
        <StatusCard lines={lines} />
      </div>

      <div className="grid gap-5 md:grid-cols-2">
        <AddressList addresses={data.addresses} />

        <Card title={t("account.details.title")}>
          <KeyValue label={t("account.details.login")} value={data.login} />
          <KeyValue label={t("account.details.role")} value={t(`userMenu.role.${data.role}`)} />
          <KeyValue label={t("account.details.created")} value={formatDate(data.createdAt, language)} />
          <KeyValue
            label={t("account.storage.title")}
            value={
              data.quotaBytes > 0
                ? t("account.storage.usedOf", {
                    used: formatBytes(data.usedBytes, language),
                    quota: formatBytes(data.quotaBytes, language),
                  })
                : t("account.storage.used", { used: formatBytes(data.usedBytes, language) })
            }
          />
        </Card>

        <PictureCard
          endpoint="/api/account/picture"
          title={t("pictures.own.title")}
          intro={t("pictures.own.intro")}
          face
          className="md:col-span-2"
        />

        <Card id="apps" title={t("account.apps.title")} className="scroll-mt-6 md:col-span-2">
          <KeyValue label={t("account.apps.server")} value={hostname} copy={hostname} />
          <KeyValue label={t("account.apps.jmap")} value={`https://${hostname}`} copy={`https://${hostname}`} />
          <KeyValue label={t("account.apps.imap")} value={t("account.apps.imapValue", { hostname })} />
          <KeyValue label={t("account.apps.submission")} value={t("account.apps.submissionValue", { hostname })} />
          <KeyValue label={t("account.apps.username")} value={data.login} copy={data.login} />
          <AppleProfile />
        </Card>
      </div>
    </div>
  );
}

const EMPTY_PROFILE: Profile = {
  login: "",
  name: "",
  role: "user",
  addresses: [],
  quotaBytes: 0,
  usedBytes: 0,
  createdAt: 0,
};
