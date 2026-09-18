import clsx from "clsx";
import {
  AtSign,
  ChartNoAxesColumn,
  DatabaseBackup,
  ChevronsUpDown,
  Download,
  Forward,
  Globe,
  History,
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
import type { LucideIcon } from "lucide-react";
import { IconButton } from "@/components/ui/Button";
import { Segmented } from "@/components/ui/Field";
import { Wordmark } from "@/components/ui/Logo";
import { Menu } from "@/components/ui/Menu";
import { useT } from "@/i18n";
import type { Session } from "@/lib/api";
import { Link, usePath } from "@/lib/router";
import { useLogout, useSavePrefs } from "@/features/session/session";
import { usePrefs, type Mode } from "@/state/prefs";
import { AppearanceDialog } from "./AppearanceDialog";

interface NavItem {
  to: string;
  label: string;
  icon: LucideIcon;
}

function NavSection({
  title,
  icon: Icon,
  items,
  path,
  onNavigate,
}: {
  title: string;
  icon: LucideIcon;
  items: NavItem[];
  path: string;
  onNavigate: () => void;
}) {
  return (
    <div className="flex flex-col gap-0.5">
      <p className="flex items-center gap-2 px-3 pt-4 pb-1.5 text-[12px] font-semibold tracking-wide text-faint uppercase">
        <Icon className="size-3.5" aria-hidden />
        {title}
      </p>
      {items.map((item) => {
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

function ModeSwitch() {
  const { t } = useT();
  const mode = usePrefs((s) => s.mode);
  const save = useSavePrefs();
  return (
    <Segmented<Mode>
      label={t("mode.label")}
      value={mode}
      onChange={(value) => save.mutate({ mode: value })}
      options={[
        { value: "simple", label: t("mode.simple") },
        { value: "pro", label: t("mode.pro") },
      ]}
    />
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
  const pro = usePrefs((s) => s.mode) === "pro";

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
        <NavSection
          title={t("nav.account")}
          icon={UserRound}
          path={path}
          onNavigate={() => setDrawer(false)}
          items={[
            { to: "/account", label: t("nav.overview"), icon: LayoutDashboard },
            { to: "/account/addresses", label: t("nav.addresses"), icon: AtSign },
            { to: "/account/mail", label: t("nav.mail"), icon: Forward },
            { to: "/account/spam", label: t("nav.spamFilter"), icon: MailWarning },
            { to: "/account/security", label: t("nav.security"), icon: ShieldCheck },
          ]}
        />
        {isAdmin && (
          <NavSection
            title={t("nav.server")}
            icon={Server}
            path={path}
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
              { to: "/admin/setup", label: t("nav.setup"), icon: WandSparkles },
              { to: "/admin/log", label: t("nav.log"), icon: History },
              // The raw server log is for Pro mode.
              ...(pro ? [{ to: "/admin/logs", label: t("nav.logs"), icon: ScrollText }] : []),
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
        {/* On a phone the switch is the last thing that fits, and the same setting sits in the
            appearance dialog, which the menu at the foot of the drawer opens. */}
        <div className="ml-auto max-sm:hidden">
          <ModeSwitch />
        </div>
      </header>

      <main className="mx-auto w-full max-w-[1080px] px-4 py-6 sm:px-6 sm:py-8">{children}</main>
      <AppearanceDialog open={appearance} onClose={() => setAppearance(false)} />
    </div>
  );
}
