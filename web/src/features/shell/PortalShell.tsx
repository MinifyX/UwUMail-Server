import { AvatarBlob, IconButton, Menu, Tag, ICONS, Icon, cx } from "@uwusuite/design";
import { useQuery } from "@tanstack/react-query";
import {
  AtSign,
  ChevronsUpDown,
  CircleFadingArrowUp,
  Inbox,
  ScrollText,
  MailWarning,
  Palette,
  ShieldBan,
  Truck,
} from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import type { LucideIcon } from "lucide-react";
import { Wordmark } from "@/components/ui/Logo";
import { useT } from "@/i18n";
import { toast } from "@/state/toasts";
import { api, type PictureState, type Session } from "@/lib/api";
import { Link, usePath } from "@/lib/router";
import { adminViewOf, inCalmNav } from "@/features/admin/adminPrefs";
import { useLogout } from "@/features/session/session";
import { AppearanceDialog } from "./AppearanceDialog";
import { seenFeaturesOf, showsNew, useMarkSeen } from "./newBadge";

interface NavItem {
  to: string;
  label: string;
  icon: LucideIcon;
  /** Further addresses this entry stands for, such as the tabs of a page gathered under it. */
  also?: string[];
  /**
   * The version that brought this page. A server updated from an older version shows "New" beside
   * it until the person opens it once (docs/development.md).
   */
  newSince?: string;
  /** A small word beside the label, such as "New"; set from `newSince`. */
  tag?: string;
  /** Runs when the entry is clicked. */
  onOpen?: () => void;
}

interface NavGroup {
  title?: string;
  items: NavItem[];
}

/** Whether a menu entry is the page one is on, or the section it belongs to. */
export function isCurrent(item: Pick<NavItem, "to" | "also">, path: string): boolean {
  // The overviews only light up on their own pages, sections also on their sub-pages.
  const overview = item.to === "/admin" || item.to === "/account";
  return (
    path === item.to ||
    (!overview && path.startsWith(`${item.to}/`)) ||
    (item.also ?? []).some((also) => path === also || path.startsWith(`${also}/`))
  );
}

/** The two halves of the portal: one's own account, and (for admins) the server. */
export type Mode = "account" | "server";

export function modeOf(path: string): Mode {
  return path === "/admin" || path.startsWith("/admin/") ? "server" : "account";
}

function NavRow({ item, path, onNavigate }: { item: NavItem; path: string; onNavigate: () => void }) {
  const active = isCurrent(item, path);
  return (
    <Link
      to={item.to}
      onClick={() => {
        item.onOpen?.();
        onNavigate();
      }}
      aria-current={active ? "page" : undefined}
      className={cx(
        "flex h-9 items-center gap-3 rounded-xl px-3 text-sm font-semibold transition-colors",
        active ? "bg-pink-tint text-pink-ink" : "text-muted hover:bg-pink-tint/50 hover:text-ink",
      )}
    >
      <item.icon className="size-[18px] shrink-0" strokeWidth={2} aria-hidden />
      <span className="truncate">{item.label}</span>
      {item.tag && (
        <Tag tone="pink" className="ml-auto h-[18px] px-1.5 text-[10px]">
          {item.tag}
        </Tag>
      )}
    </Link>
  );
}

function NavGroups({ groups, path, onNavigate }: { groups: NavGroup[]; path: string; onNavigate: () => void }) {
  return (
    <>
      {groups.map((group, index) => (
        <div key={group.title ?? index} className="flex flex-col gap-0.5">
          {group.title && (
            <p
              className={cx(
                "px-3 pb-1.5 text-[12px] font-bold tracking-wide text-muted uppercase",
                index === 0 ? "pt-1" : "pt-4",
              )}
            >
              {group.title}
            </p>
          )}
          {group.items.map((item) => (
            <NavRow key={item.to} item={item} path={path} onNavigate={onNavigate} />
          ))}
        </div>
      ))}
    </>
  );
}

/** "My account | Server" at the top of the menu; only admins have the second half. */
function ModeSwitch({ mode, onNavigate }: { mode: Mode; onNavigate: () => void }) {
  const { t } = useT();
  const options: { value: Mode; label: string; to: string }[] = [
    { value: "account", label: t("nav.account"), to: "/account" },
    { value: "server", label: t("nav.server"), to: "/admin" },
  ];
  return (
    <nav aria-label={t("nav.mode")} className="flex rounded-full bg-canvas p-1">
      {options.map((option) => (
        <Link
          key={option.value}
          to={option.to}
          onClick={onNavigate}
          aria-current={mode === option.value ? "true" : undefined}
          className={cx(
            "flex h-8 flex-1 items-center justify-center rounded-full px-3 text-[13px] font-semibold whitespace-nowrap transition-colors",
            mode === option.value ? "bg-surface text-pink-ink shadow-sm" : "text-muted hover:text-ink",
          )}
        >
          {option.label}
        </Link>
      ))}
    </nav>
  );
}

function useNavigation(session: Session) {
  const { t } = useT();
  const isAdmin = session.account.role === "admin";
  const calm = isAdmin && adminViewOf(session.preferences) === "simple";
  const seen = seenFeaturesOf(session.preferences);
  const markSeen = useMarkSeen();
  // The id an entry is remembered by once opened, such as "account-codes".
  const withTag = (item: NavItem): NavItem => {
    const id = item.to.slice(1).replaceAll("/", "-");
    if (!item.newSince || !showsNew({ id, since: item.newSince }, session.server.installedVersion, seen)) return item;
    // The server keeps at most 64; the oldest go first.
    return { ...item, tag: t("nav.new"), onOpen: () => markSeen.mutate([...seen, id].slice(-64)) };
  };

  const account: NavGroup[] = [
    {
      title: t("nav.groups.account"),
      items: [
        { to: "/account", label: t("nav.overview"), icon: ICONS.overview },
        { to: "/account/addresses", label: t("nav.addresses"), icon: AtSign },
        { to: "/account/masked", label: t("nav.masked"), icon: ICONS.hide },
        { to: "/account/security", label: t("nav.security"), icon: ICONS.securityCheck },
      ],
    },
    {
      title: t("nav.groups.mail"),
      items: [
        { to: "/account/mail", label: t("nav.mail"), icon: ICONS.forward },
        { to: "/account/spam", label: t("nav.spamFilter"), icon: MailWarning },
        { to: "/account/fetch", label: t("nav.fetch"), icon: ICONS.download },
        { to: "/account/moving", label: t("nav.moving"), icon: Truck },
        { to: "/account/codes", label: t("nav.codes"), icon: ICONS.secret, newSince: "0.26.0" },
      ],
    },
    {
      title: t("nav.groups.extras"),
      items: [
        { to: "/account/calendars", label: t("nav.calendars"), icon: ICONS.calendar },
        { to: "/account/assist", label: t("nav.assist"), icon: ICONS.ai },
      ],
    },
  ].map((group) => ({ ...group, items: group.items.map(withTag) }));

  const serverItems: NavItem[] = [
    {
      to: "/admin",
      label: t("nav.overview"),
      icon: ICONS.overview,
      also: ["/admin/stats", "/admin/mail-flow", "/admin/microsoft", "/admin/backups"],
    },
    {
      to: "/admin/people",
      label: t("nav.directory"),
      icon: ICONS.people,
      also: ["/admin/domains", "/admin/moves", "/admin/reports"],
    },
    { to: "/admin/queue", label: t("nav.queue"), icon: ICONS.send },
    { to: "/admin/spam", label: t("nav.spamFilter"), icon: ShieldBan },
    { to: "/admin/settings", label: t("nav.settings"), icon: ICONS.settings },
    { to: "/admin/updates", label: t("nav.updates"), icon: CircleFadingArrowUp },
    { to: "/admin/logs", label: t("nav.logs"), icon: ScrollText },
  ].map(withTag);
  // The calm view keeps the rest one click away instead of hiding it.
  const server: NavGroup[] = calm
    ? [
        { items: serverItems.filter((item) => inCalmNav(item.to)) },
        { title: t("nav.more"), items: serverItems.filter((item) => !inCalmNav(item.to)) },
      ]
    : [{ items: serverItems }];

  return { isAdmin, account, server };
}

function UserMenu({
  session,
  onAppearance,
  onNavigate,
}: {
  session: Session;
  onAppearance: () => void;
  onNavigate: () => void;
}) {
  const { t } = useT();
  const logout = useLogout();
  const name = session.account.name || session.account.login;
  // Same key as the picture card, so a new picture shows here at once.
  const picture = useQuery({
    queryKey: ["picture", "/api/account/picture"],
    queryFn: () => api<PictureState>("/api/account/picture"),
  }).data?.picture;
  return (
    <Menu
      align="start"
      side="above"
      className="w-full"
      items={[
        // A real address, not a route: the webmail is its own app under /mail.
        ...(session.webmail
          ? [{ icon: Inbox, label: t("nav.openMailbox"), onSelect: () => window.location.assign("/mail") }]
          : []),
        {
          icon: Palette,
          label: t("userMenu.appearance"),
          onSelect: () => {
            onNavigate();
            onAppearance();
          },
        },
        {
          icon: ICONS.signOut,
          label: t("userMenu.logout"),
          onSelect: () => logout.mutate(undefined, { onError: () => toast(t("userMenu.logoutFailed"), "error") }),
        },
      ]}
      trigger={(props) => (
        <button
          type="button"
          onClick={props.toggle}
          aria-haspopup={props["aria-haspopup"]}
          aria-expanded={props["aria-expanded"]}
          aria-controls={props["aria-controls"]}
          aria-label={t("userMenu.open")}
          className="flex min-h-[52px] w-full items-center gap-3 rounded-xl border border-hairline p-2 text-left transition-colors hover:bg-pink-tint/50"
        >
          <span aria-hidden className="flex size-8 shrink-0 overflow-hidden rounded-full">
            {picture ? (
              <img src={picture.url} alt="" className="size-full object-cover" />
            ) : (
              <AvatarBlob seed={session.account.login} />
            )}
          </span>
          <span className="min-w-0 flex-1 leading-tight">
            <span className="block truncate text-sm font-semibold">{name}</span>
            <span className="block truncate text-[12px] text-muted">
              {session.account.role === "admin" ? t(`userMenu.role.admin`) : session.account.login}
            </span>
          </span>
          <Icon icon={ChevronsUpDown} className="size-4 text-faint" aria-hidden />
        </button>
      )}
    />
  );
}

interface TabItem {
  to?: string;
  label: string;
  icon: LucideIcon;
  current: boolean;
}

/** The phone's tab bar: the most used pages of the current half, and "More" for everything. */
function TabBar({ items, onMore, moreOpen }: { items: TabItem[]; onMore: () => void; moreOpen: boolean }) {
  const { t } = useT();
  return (
    <nav
      aria-label={t("nav.label")}
      className="fixed inset-x-0 bottom-0 z-30 border-t border-hairline bg-tabbar pb-[env(safe-area-inset-bottom)] min-[700px]:hidden"
    >
      <ul className="mx-auto flex max-w-[520px] px-[max(8px,env(safe-area-inset-left))]">
        {items.map((item) => {
          const content = (
            <>
              <span
                className={cx(
                  "flex h-7 w-14 items-center justify-center rounded-full transition-colors",
                  item.current && "bg-pink-tint",
                )}
              >
                <item.icon className="size-[22px]" strokeWidth={item.current ? 2.3 : 2} aria-hidden />
              </span>
              <span className="truncate">{item.label}</span>
            </>
          );
          const className = cx(
            "flex min-h-14 w-full flex-col items-center justify-center gap-0.5 pt-1.5 pb-1 text-[11px] font-semibold",
            item.current ? "text-pink-ink" : "text-muted",
          );
          return (
            <li key={item.label} className="flex-1">
              {item.to ? (
                <Link to={item.to} aria-current={item.current ? "page" : undefined} className={className}>
                  {content}
                </Link>
              ) : (
                <button
                  type="button"
                  onClick={onMore}
                  aria-expanded={moreOpen}
                  aria-label={t("nav.more")}
                  className={className}
                >
                  {content}
                </button>
              )}
            </li>
          );
        })}
      </ul>
    </nav>
  );
}

export function PortalShell({ session, children }: { session: Session; children: ReactNode }) {
  const { t } = useT();
  const path = usePath();
  const [more, setMore] = useState(false);
  const [appearance, setAppearance] = useState(false);
  const { isAdmin, account, server } = useNavigation(session);
  const mode: Mode = isAdmin ? modeOf(path) : "account";
  const groups = mode === "server" ? server : account;
  const close = () => setMore(false);

  useEffect(() => {
    if (!more) return;
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && setMore(false);
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [more]);

  const tabTargets: { to: string; label: string; icon: LucideIcon; also?: string[] }[] =
    mode === "server"
      ? [
          { to: "/admin", label: t("nav.tabs.server"), icon: ICONS.overview, also: server[0]?.items[0]?.also },
          { to: "/admin/people", label: t("nav.tabs.people"), icon: ICONS.people, also: server[0]?.items[1]?.also },
          { to: "/admin/queue", label: t("nav.tabs.queue"), icon: ICONS.send },
        ]
      : [
          { to: "/account", label: t("nav.overview"), icon: ICONS.overview },
          { to: "/account/addresses", label: t("nav.addresses"), icon: AtSign },
          { to: "/account/codes", label: t("nav.tabs.codes"), icon: ICONS.secret },
        ];
  const tabs: TabItem[] = tabTargets.map((target) => ({ ...target, current: !more && isCurrent(target, path) }));
  const inTabs = tabs.some((tab) => tab.current);
  tabs.push({ label: t("nav.more"), icon: ICONS.more, current: more || !inTabs });

  const menu = (inSheet: boolean) => (
    <div className="flex h-full flex-col">
      {isAdmin && (
        <div className="px-0.5 pb-4">
          <ModeSwitch mode={mode} onNavigate={close} />
        </div>
      )}
      <nav aria-label={t("nav.label")} className={cx("flex flex-col", !inSheet && "flex-1 overflow-y-auto")}>
        <NavGroups groups={groups} path={path} onNavigate={close} />
      </nav>
      <div className="mt-auto pt-3">
        <UserMenu session={session} onAppearance={() => setAppearance(true)} onNavigate={close} />
      </div>
    </div>
  );

  return (
    <div className="min-h-screen min-[700px]:pl-64">
      <aside className="fixed inset-y-0 left-0 hidden w-64 flex-col border-r border-hairline bg-surface px-3 pt-[18px] pb-3 min-[700px]:flex">
        <Link
          to={mode === "server" ? "/admin" : "/account"}
          className="mb-4 self-start rounded-full px-2.5"
          aria-label={t("nav.home")}
        >
          <Wordmark className="text-lg" />
        </Link>
        <div className="min-h-0 flex-1">{menu(false)}</div>
      </aside>

      <main
        className={cx(
          "mx-auto w-full max-w-[1080px] px-4 min-[700px]:py-8 sm:px-6 lg:px-10",
          // On the phone: below the status bar and above the tab bar.
          "max-[699px]:pt-[max(16px,env(safe-area-inset-top))] max-[699px]:pb-[calc(88px+env(safe-area-inset-bottom))]",
          "max-[699px]:pr-[max(16px,env(safe-area-inset-right))] max-[699px]:pl-[max(16px,env(safe-area-inset-left))]",
        )}
      >
        {children}
      </main>

      <TabBar items={tabs} moreOpen={more} onMore={() => setMore((open) => !open)} />

      {more && (
        <div className="fixed inset-0 z-40 min-[700px]:hidden">
          {/* A tap beside the sheet leaves it open; the X, Escape or a choice closes it. */}
          <div aria-hidden className="absolute inset-0 animate-fade bg-scrim" />
          <div
            role="dialog"
            aria-modal="true"
            aria-label={t("nav.more")}
            className="absolute inset-x-0 bottom-0 flex max-h-[85svh] animate-slide-up flex-col rounded-t-dialog bg-surface px-3 pt-2 pb-[calc(12px+env(safe-area-inset-bottom))] shadow-float"
          >
            <div className="flex items-center justify-between px-2.5 pb-3">
              <Wordmark className="text-base" />
              <IconButton icon={ICONS.close} label={t("nav.closeMenu")} onClick={close} />
            </div>
            <div className="min-h-0 overflow-y-auto">{menu(true)}</div>
          </div>
        </div>
      )}
      <AppearanceDialog open={appearance} onClose={() => setAppearance(false)} />
    </div>
  );
}
