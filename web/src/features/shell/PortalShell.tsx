import clsx from "clsx";
import {
  AtSign,
  ChartNoAxesColumn,
  EarthLock,
  DatabaseBackup,
  ChevronsUpDown,
  Download,
  Forward,
  Globe,
  History,
  Inbox,
  ScrollText,
  Send,
  Settings,
  LayoutDashboard,
  LogOut,
  MailWarning,
  Menu as MenuIcon,
  Palette,
  Server,
  ShieldBan,
  ShieldCheck,
  UserRound,
  Users,
  WandSparkles,
  X,
} from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { ChevronDown } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { IconButton } from "@/components/ui/Button";
import { Wordmark } from "@/components/ui/Logo";
import { Menu } from "@/components/ui/Menu";
import { useT } from "@/i18n";
import type { Session } from "@/lib/api";
import { Link, usePath } from "@/lib/router";
import { useLogout } from "@/features/session/session";
import { AppearanceDialog } from "./AppearanceDialog";

interface NavItem {
  to: string;
  label: string;
  icon: LucideIcon;
}

const OPEN_SECTIONS_KEY = "uwumail-portal-nav";

/** Which menu groups this browser last left open. A group nobody touched is not in here. */
function storedOpen(): Record<string, boolean> {
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(OPEN_SECTIONS_KEY) ?? "{}");
    if (!raw || typeof raw !== "object") return {};
    return Object.fromEntries(
      Object.entries(raw as Record<string, unknown>).filter(([, value]) => typeof value === "boolean"),
    ) as Record<string, boolean>;
  } catch {
    // Private windows may refuse storage; then every group simply starts at its default.
    return {};
  }
}

function rememberOpen(name: string, open: boolean) {
  try {
    localStorage.setItem(OPEN_SECTIONS_KEY, JSON.stringify({ ...storedOpen(), [name]: open }));
  } catch {
    // See above: without storage the choice holds for this visit only.
  }
}

function NavSection({
  name,
  title,
  icon: Icon,
  items,
  path,
  onNavigate,
  collapsible,
  startsOpen,
}: {
  /** How this group is remembered; only needed when it can be folded away. */
  name?: string;
  title: string;
  icon: LucideIcon;
  items: NavItem[];
  path: string;
  onNavigate: () => void;
  collapsible?: boolean;
  startsOpen?: boolean;
}) {
  const { t } = useT();
  const here = items.some((item) => path === item.to || path.startsWith(`${item.to}/`));
  // What this browser last chose wins; otherwise the group opens if the current page is in it.
  const [open, setOpen] = useState(() => {
    const remembered = name ? storedOpen()[name] : undefined;
    return remembered ?? (here || (startsOpen ?? true));
  });
  // Landing inside a folded group unfolds it, so the current page is never hidden. Only the
  // arrival counts: staying here after folding it away by hand leaves it folded.
  const [wasHere, setWasHere] = useState(here);
  if (here !== wasHere) {
    setWasHere(here);
    if (here) setOpen(true);
  }
  const toggle = () => {
    const next = !open;
    setOpen(next);
    if (name) rememberOpen(name, next);
  };
  const heading = (
    <>
      <Icon className="size-3.5" aria-hidden />
      {title}
    </>
  );

  return (
    <div className="flex flex-col gap-0.5">
      {collapsible ? (
        <button
          type="button"
          onClick={toggle}
          aria-expanded={open}
          aria-label={t(open ? "nav.collapse" : "nav.expand", { group: title })}
          className="flex items-center gap-2 rounded-full px-3 pt-4 pb-1.5 text-[12px] font-semibold tracking-wide text-faint uppercase hover:text-muted"
        >
          {heading}
          <ChevronDown className={clsx("size-3.5 transition-transform", !open && "-rotate-90")} aria-hidden />
        </button>
      ) : (
        <p className="flex items-center gap-2 px-3 pt-4 pb-1.5 text-[12px] font-semibold tracking-wide text-faint uppercase">
          {heading}
        </p>
      )}
      {open &&
        items.map((item) => {
          // The overview only lights up on its own page, sections also on their sub-pages.
          const overview = item.to === "/admin" || item.to === "/account";
          const active = path === item.to || (!overview && path.startsWith(`${item.to}/`));
          return (
            <Link
              key={item.to}
              to={item.to}
              onClick={onNavigate}
              aria-current={active ? "page" : undefined}
              className={clsx(
                "flex h-10 items-center gap-3 rounded-full px-3 text-sm font-semibold transition-colors",
                active ? "bg-pink-tint text-pink-ink" : "text-muted hover:bg-pink-tint/50 hover:text-ink",
              )}
            >
              <item.icon className="size-[18px]" strokeWidth={2} aria-hidden />
              {item.label}
            </Link>
          );
        })}
    </div>
  );
}

export function PortalShell({ session, children }: { session: Session; children: ReactNode }) {
  const { t } = useT();
  const path = usePath();
  const logout = useLogout();
  const [drawer, setDrawer] = useState(false);
  /** Counts up whenever the phone menu opens, which makes Nyu hop once at the top of it. */
  const [hops, setHops] = useState(0);
  const [appearance, setAppearance] = useState(false);
  const isAdmin = session.account.role === "admin";
  useEffect(() => {
    if (!drawer) return;
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && setDrawer(false);
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [drawer]);

  const sidebar = (
    <nav aria-label={t("nav.label")} className="flex h-full flex-col px-3 pt-5 pb-3">
      <div className="flex items-center justify-between px-3">
        <Link to="/account" onClick={() => setDrawer(false)} className="rounded-full">
          <Wordmark className="text-lg" hop={hops} />
        </Link>
        <IconButton icon={X} label={t("nav.closeMenu")} className="lg:hidden" onClick={() => setDrawer(false)} />
      </div>
      <div className="mt-2 flex-1 overflow-y-auto">
        {session.webmail && (
          // A real link, not a route: the webmail is its own app under /mail.
          <a
            href="/mail"
            onClick={() => setDrawer(false)}
            className="mt-2 flex h-10 items-center gap-3 rounded-full bg-pink-tint px-3 text-sm font-semibold text-pink-ink transition-colors hover:bg-pink-tint/70"
          >
            <Inbox className="size-[18px]" strokeWidth={2} aria-hidden />
            {t("nav.mailbox")}
          </a>
        )}
        <NavSection
          name="account"
          title={t("nav.account")}
          icon={UserRound}
          path={path}
          collapsible={isAdmin}
          onNavigate={() => setDrawer(false)}
          items={[
            { to: "/account", label: t("nav.overview"), icon: LayoutDashboard },
            { to: "/account/addresses", label: t("nav.addresses"), icon: AtSign },
            { to: "/account/mail", label: t("nav.mail"), icon: Forward },
            { to: "/account/fetch", label: t("nav.fetch"), icon: Download },
            { to: "/account/spam", label: t("nav.spamFilter"), icon: MailWarning },
            { to: "/account/security", label: t("nav.security"), icon: ShieldCheck },
          ]}
        />
        {isAdmin && (
          <NavSection
            name="server"
            title={t("nav.server")}
            icon={Server}
            path={path}
            collapsible
            startsOpen={false}
            onNavigate={() => setDrawer(false)}
            items={[
              { to: "/admin", label: t("nav.overview"), icon: LayoutDashboard },
              { to: "/admin/people", label: t("nav.people"), icon: Users },
              { to: "/admin/domains", label: t("nav.domains"), icon: Globe },
              { to: "/admin/reports", label: t("nav.reports"), icon: ChartNoAxesColumn },
              { to: "/admin/queue", label: t("nav.queue"), icon: Send },
              { to: "/admin/spam", label: t("nav.spamFilter"), icon: ShieldBan },
              { to: "/admin/backups", label: t("nav.backups"), icon: DatabaseBackup },
              { to: "/admin/updates", label: t("nav.updates"), icon: Download },
              { to: "/admin/settings", label: t("nav.settings"), icon: Settings },
              { to: "/admin/vpn", label: t("nav.vpn"), icon: EarthLock },
              { to: "/admin/setup", label: t("nav.setup"), icon: WandSparkles },
              { to: "/admin/log", label: t("nav.log"), icon: History },
              { to: "/admin/logs", label: t("nav.logs"), icon: ScrollText },
            ]}
          />
        )}
      </div>
      <Menu
        align="start"
        className="w-full [&>[role=menu]]:top-auto [&>[role=menu]]:bottom-[calc(100%+6px)]"
        items={[
          {
            label: (
              <span className="flex items-center gap-2">
                <Palette className="size-4" aria-hidden />
                {t("userMenu.appearance")}
              </span>
            ),
            onSelect: () => {
              setDrawer(false);
              setAppearance(true);
            },
          },
          {
            label: (
              <span className="flex items-center gap-2">
                <LogOut className="size-4" aria-hidden />
                {t("userMenu.logout")}
              </span>
            ),
            onSelect: () => logout.mutate(),
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
            className="flex w-full items-center gap-3 rounded-2xl p-2 text-left transition-colors hover:bg-pink-tint/50"
          >
            <span className="flex size-9 shrink-0 items-center justify-center rounded-full bg-pink-tint text-sm font-bold text-pink-ink">
              {(session.account.name || session.account.login).slice(0, 1).toUpperCase()}
            </span>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm font-semibold">
                {session.account.name || session.account.login}
              </span>
              <span className="block truncate text-[12px] text-muted">
                {t(`userMenu.role.${session.account.role}`)}
              </span>
            </span>
            <ChevronsUpDown className="size-4 text-faint" aria-hidden />
          </button>
        )}
      />
    </nav>
  );

  return (
    <div className="min-h-screen lg:pl-[264px]">
      <aside className="fixed inset-y-0 left-0 hidden w-[264px] border-r border-hairline bg-surface lg:block">
        {sidebar}
      </aside>

      {drawer && (
        <div className="fixed inset-0 z-50 lg:hidden">
          <button
            type="button"
            aria-label={t("nav.closeMenu")}
            className="absolute inset-0 animate-fade bg-[#1c1420]/35"
            onClick={() => setDrawer(false)}
          />
          <aside className="absolute inset-y-0 left-0 w-[min(300px,85vw)] animate-drawer bg-surface shadow-float">
            {sidebar}
          </aside>
        </div>
      )}

      <header className="sticky top-0 z-30 flex h-16 items-center gap-3 border-b border-hairline bg-canvas/85 px-4 backdrop-blur sm:px-6">
        <IconButton
          icon={MenuIcon}
          label={t("nav.openMenu")}
          className="lg:hidden"
          onClick={() => {
            setDrawer(true);
            setHops((count) => count + 1);
          }}
        />
        <Link to="/account" className="rounded-full lg:hidden">
          <Wordmark className="text-base" />
        </Link>
      </header>

      <main className="mx-auto w-full max-w-[1080px] px-4 py-6 sm:px-6 sm:py-8">{children}</main>
      <AppearanceDialog open={appearance} onClose={() => setAppearance(false)} />
    </div>
  );
}
