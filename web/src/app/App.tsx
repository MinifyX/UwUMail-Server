import { useEffect, type ReactNode } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { EmptyState } from "@/components/ui/EmptyState";
import { Toaster } from "@/components/ui/Toaster";
import { AccountHome } from "@/features/account/AccountHome";
import { FetchPage } from "@/features/fetch/FetchPage";
import { AddressesPage } from "@/features/addresses/AddressesPage";
import { SERVER_PATHS, ServerPage, type ServerTab } from "@/features/admin/ServerPage";
import { DomainPage } from "@/features/domains/DomainPage";
import { DomainsPage } from "@/features/domains/DomainsPage";
import { ForwardConfirmPage } from "@/features/mailbox/ForwardConfirmPage";
import { MailboxPage } from "@/features/mailbox/MailboxPage";
import { LoginPage } from "@/features/login/LoginPage";
import { PROTOCOLS_PATHS, ProtocolsPage, type ProtocolsTab } from "@/features/logs/ProtocolsPage";
import { PasswordPage } from "@/features/password/PasswordPage";
import { PeoplePage } from "@/features/people/PeoplePage";
import { PersonPage } from "@/features/people/PersonPage";
import { QueuePage } from "@/features/queue/QueuePage";
import { ReportsPage } from "@/features/reports/ReportsPage";
import { SecurityPage } from "@/features/security/SecurityPage";
import { AdminSettingsPage, SETTINGS_PATHS, type AdminSettingsTab } from "@/features/settings/AdminSettingsPage";
import { SetupWizard } from "@/features/setup/SetupWizard";
import { AccountSpamPage, AdminSpamPage, SPAM_TABS, type SpamTab } from "@/features/spam/SpamPage";
import { useSession } from "@/features/session/session";
import { PortalShell } from "@/features/shell/PortalShell";
import { useApplyLanguage, useT } from "@/i18n";
import type { Session } from "@/lib/api";
import { matchPath, navigate, usePath } from "@/lib/router";
import { useApplyTheme } from "@/lib/theme";

function NotFound() {
  const { t } = useT();
  return (
    <EmptyState
      scene="search"
      title={t("errors.notFound.title")}
      body={t("errors.notFound.body")}
      action={
        <Button variant="primary" onClick={() => navigate("/account")}>
          {t("errors.notFound.home")}
        </Button>
      }
    />
  );
}

/** An address that moved: replaced in the history, so going back does not land here again. */
function Redirect({ to }: { to: string }) {
  useEffect(() => navigate(to, { replace: true }), [to]);
  return null;
}

/** Addresses from before pages were gathered into tabs, as bookmarks and old mails still have them. */
const MOVED: Record<string, string> = {
  "/admin/setup": SERVER_PATHS.mailFlow,
  "/admin/log": PROTOCOLS_PATHS.changes,
  "/admin/vpn": SETTINGS_PATHS.vpn,
};

/** Which tab of a tabbed page an address opens, if any. */
function tabOf<T extends string>(paths: Record<T, string>, path: string): T | undefined {
  return (Object.keys(paths) as T[]).find((tab) => matchPath(paths[tab], path));
}

/** Pages inside the portal; admin pages only exist for admins. */
function page(path: string, session: Session): ReactNode {
  if (path === "/account") return <AccountHome session={session} />;
  if (path === "/account/security") return <SecurityPage session={session} />;
  if (path === "/account/mail") return <MailboxPage />;
  if (path === "/account/fetch") return <FetchPage />;
  if (path === "/account/addresses") return <AddressesPage />;
  if (path === "/account/spam") return <AccountSpamPage />;
  if (path === "/account/spam/lists") return <AccountSpamPage tab="lists" />;
  if (path === "/account/spam/learning") return <AccountSpamPage tab="learning" />;
  if (path === "/account/spam/waiting") return <AccountSpamPage tab="waiting" />;
  if (session.account.role !== "admin") return <NotFound />;
  const moved = MOVED[path];
  if (moved) return <Redirect to={moved} />;
  const serverTab = tabOf<ServerTab>(SERVER_PATHS, path);
  if (serverTab) return <ServerPage tab={serverTab} session={session} />;
  if (matchPath("/admin/people", path)) return <PeoplePage session={session} />;
  const person = matchPath("/admin/people/:login", path);
  if (person?.login) return <PersonPage key={person.login} login={person.login} session={session} />;
  if (matchPath("/admin/domains", path)) return <DomainsPage />;
  const domain = matchPath("/admin/domains/:name", path);
  if (domain?.name) return <DomainPage key={domain.name} name={domain.name} />;
  if (matchPath("/admin/queue", path)) return <QueuePage />;
  if (matchPath("/admin/reports", path)) return <ReportsPage />;
  if (matchPath("/admin/spam", path)) return <AdminSpamPage />;
  const spamTab = matchPath("/admin/spam/:tab", path)?.tab;
  if (spamTab && SPAM_TABS.includes(spamTab as SpamTab)) return <AdminSpamPage tab={spamTab as SpamTab} />;
  const protocolsTab = tabOf<ProtocolsTab>(PROTOCOLS_PATHS, path);
  if (protocolsTab) return <ProtocolsPage tab={protocolsTab} />;
  const settingsTab = tabOf<AdminSettingsTab>(SETTINGS_PATHS, path);
  if (settingsTab) return <AdminSettingsPage tab={settingsTab} />;
  return <NotFound />;
}

function Portal({ session }: { session: Session }) {
  const path = usePath();
  const entry = path === "/" || path === "/login";

  useEffect(() => {
    if (entry) navigate("/account", { replace: true });
  }, [entry]);

  return <PortalShell session={session}>{page(entry ? "/account" : path, session)}</PortalShell>;
}

function Routes() {
  const path = usePath();
  const session = useSession();
  const passwordLink = matchPath("/password/:token", path);
  const forwardingLink = matchPath("/forwarding/:token", path);

  // Choosing a password works whether someone is logged in or not.
  if (passwordLink?.token) return <PasswordPage token={passwordLink.token} />;
  // Whoever owns the forwarding address may have no account here at all.
  if (forwardingLink?.token) return <ForwardConfirmPage token={forwardingLink.token} />;
  if (session.isPending) return <Loading fullPage />;
  if (session.isError) {
    return (
      <main className="flex min-h-screen items-center justify-center">
        <LoadError error={session.error} onRetry={() => void session.refetch()} />
      </main>
    );
  }
  // The assistant stays on screen while its second step logs the new admin in.
  if (path === "/setup") return <SetupWizard session={session.data} />;
  if (!session.data) return <LoginPage />;
  return <Portal session={session.data} />;
}

export function App() {
  useApplyTheme();
  useApplyLanguage();
  return (
    <>
      <Routes />
      <Toaster />
    </>
  );
}
