import { useEffect } from "react";
import { Button } from "@/components/ui/Button";
import { EmptyState } from "@/components/ui/EmptyState";
import { LoadError, Loading } from "@/components/StatusViews";
import { AccountHome } from "@/features/account/AccountHome";
import { AdminHome } from "@/features/admin/AdminHome";
import { LoginPage } from "@/features/login/LoginPage";
import { useSession } from "@/features/session/session";
import { PortalShell } from "@/features/shell/PortalShell";
import { useApplyLanguage, useT } from "@/i18n";
import type { Session } from "@/lib/api";
import { navigate, usePath } from "@/lib/router";
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

function Portal({ session }: { session: Session }) {
  const path = usePath();
  const entry = path === "/" || path === "/login" || path === "/setup";

  useEffect(() => {
    if (entry) navigate("/account", { replace: true });
  }, [entry]);

  let page;
  if (entry || path === "/account") page = <AccountHome session={session} />;
  else if (path === "/admin" && session.account.role === "admin") page = <AdminHome />;
  else page = <NotFound />;

  return <PortalShell session={session}>{page}</PortalShell>;
}

export function App() {
  useApplyTheme();
  useApplyLanguage();
  const session = useSession();

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
