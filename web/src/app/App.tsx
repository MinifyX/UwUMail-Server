import { useEffect, type ReactNode } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { EmptyState } from "@/components/ui/EmptyState";
import { Toaster } from "@/components/ui/Toaster";
import { AccountHome } from "@/features/account/AccountHome";
import { AdminHome } from "@/features/admin/AdminHome";
import { LogPage } from "@/features/log/LogPage";
import { LoginPage } from "@/features/login/LoginPage";
import { PasswordPage } from "@/features/password/PasswordPage";
import { PeoplePage } from "@/features/people/PeoplePage";
import { PersonPage } from "@/features/people/PersonPage";
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
  if (session.account.role !== "admin") return <NotFound />;
  if (path === "/admin") return <AdminHome />;
  if (matchPath("/admin/people", path)) return <PeoplePage session={session} />;
  const person = matchPath("/admin/people/:login", path);
  if (person?.login) return <PersonPage key={person.login} login={person.login} session={session} />;
  if (matchPath("/admin/log", path)) return <LogPage />;
  return <NotFound />;
}

function Portal({ session }: { session: Session }) {
  const path = usePath();
  const entry = path === "/" || path === "/login" || path === "/setup";

  useEffect(() => {
    if (entry) navigate("/account", { replace: true });
  }, [entry]);

  return <PortalShell session={session}>{page(entry ? "/account" : path, session)}</PortalShell>;
}

function Routes() {
  const path = usePath();
  const session = useSession();
  const passwordLink = matchPath("/password/:token", path);

  // Choosing a password works whether someone is logged in or not.
  if (passwordLink?.token) return <PasswordPage token={passwordLink.token} />;
  if (session.isPending) return <Loading fullPage />;
  if (session.isError) {
    return (
      <main className="flex min-h-screen items-center justify-center">
        <LoadError error={session.error} onRetry={() => void session.refetch()} />
      </main>
    );
  }
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
