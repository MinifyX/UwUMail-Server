import { useEffect, type ReactNode } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { EmptyState } from "@/components/ui/EmptyState";
import { Toaster } from "@/components/ui/Toaster";
import { AccountHome } from "@/features/account/AccountHome";
import { FetchPage } from "@/features/fetch/FetchPage";
import { AddressesPage } from "@/features/addresses/AddressesPage";
import { AdminHome } from "@/features/admin/AdminHome";
import { DomainPage } from "@/features/domains/DomainPage";
import { DomainsPage } from "@/features/domains/DomainsPage";
import { LogPage } from "@/features/log/LogPage";
import { ForwardConfirmPage } from "@/features/mailbox/ForwardConfirmPage";
import { MailboxPage } from "@/features/mailbox/MailboxPage";
import { LogsPage } from "@/features/logs/LogsPage";
import { LoginPage } from "@/features/login/LoginPage";
import { PasswordPage } from "@/features/password/PasswordPage";
import { PeoplePage } from "@/features/people/PeoplePage";
import { PersonPage } from "@/features/people/PersonPage";
import { QueuePage } from "@/features/queue/QueuePage";
import { ReportsPage } from "@/features/reports/ReportsPage";
import { SecurityPage } from "@/features/security/SecurityPage";
import { SettingsPage } from "@/features/settings/SettingsPage";
import { UpdatesPage } from "@/features/updates/UpdatesPage";
import { SetupPage } from "@/features/setup/SetupPage";
import { SetupWizard } from "@/features/setup/SetupWizard";
import { AccountSpamPage, AdminSpamPage, SPAM_TABS, type SpamTab } from "@/features/spam/SpamPage";
import { VpnPage } from "@/features/vpn/VpnPage";
import { BackupsPage } from "@/features/backups/BackupsPage";
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
  if (path === "/admin") return <AdminHome />;
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
  if (matchPath("/admin/vpn", path)) return <VpnPage />;
  if (matchPath("/admin/backups", path)) return <BackupsPage />;
  if (matchPath("/admin/log", path)) return <LogPage />;
  if (matchPath("/admin/logs", path)) return <LogsPage />;
  if (matchPath("/admin/updates", path)) return <UpdatesPage />;
  if (matchPath("/admin/settings", path)) return <SettingsPage />;
  if (matchPath("/admin/setup", path)) return <SetupPage session={session} />;
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
