import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AtSign, CalendarDays, Check, IdCard, Inbox, RefreshCw, Send, UserRound, X } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useEffect, useRef, type ReactNode } from "react";
import { NyuScene } from "@/components/nyu/scenes";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { EmptyState } from "@/components/ui/EmptyState";
import { LogoSymbol, Wordmark } from "@/components/ui/Logo";
import { useT } from "@/i18n";
import { api, ApiError, setCsrfToken, type OAuthRequest, type Session } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { navigate, useSearch } from "@/lib/router";
import { decisionBody, knownScopes, type KnownScope } from "./authorize";

const SCOPE_ICONS: Record<KnownScope, LucideIcon> = {
  mail: Inbox,
  smtp: Send,
  dav: CalendarDays,
  openid: UserRound,
  profile: IdCard,
  email: AtSign,
  offline_access: RefreshCw,
};

/** The app's request itself is wrong: no use asking the person anything. */
const REFUSED = ["oauthClientUnknown", "oauthRedirectInvalid", "oauthRequestInvalid"];

const refusal = (error: unknown) => (error instanceof ApiError && REFUSED.includes(error.code) ? error.code : null);
const loggedOut = (error: unknown) => error instanceof ApiError && error.status === 401;

/** An app on this very device answers on a loopback address, like Thunderbird does. */
const onThisDevice = (host: string) => ["127.0.0.1", "[::1]", "::1", "localhost"].includes(host.toLowerCase());

function Frame({ children }: { children: ReactNode }) {
  return (
    <main className="flex min-h-screen items-center justify-center px-4 py-10">
      <div className="w-full max-w-[440px] animate-slide-up">
        <div className="mb-6 flex justify-center">
          <Wordmark className="text-2xl" />
        </div>
        {children}
      </div>
    </main>
  );
}

/** While the browser is on its way back to the app. */
function Leaving() {
  const { t } = useT();
  return (
    <Frame>
      <div role="status" className="flex flex-col items-center gap-3 py-10">
        <LogoSymbol className="nyu-blink h-14 w-auto animate-pulse" />
        <span className="text-[13px] text-muted">{t("oauth.signingIn")}</span>
      </div>
    </Frame>
  );
}

/**
 * Where a mail app asks to sign in with OAuth instead of an app password (docs/oauth.md). The
 * login comes first when needed; this page then shows who asks for what, and the server's answer
 * sends the browser back to the app, with a code or with a refusal.
 */
export function OAuthConsentPage({ session }: { session: Session }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const search = useSearch();
  const request = useQuery({
    queryKey: ["oauth", "authorize", search],
    queryFn: () => api<OAuthRequest>(`/api/oauth/authorize${search}`),
    retry: false,
    staleTime: Infinity,
  });
  const decide = useMutation({
    mutationFn: (approve: boolean) =>
      api<{ redirect: string }>("/api/oauth/authorize", { method: "POST", body: decisionBody(search, approve) }),
    onSuccess: (answer) => window.location.assign(answer.redirect),
  });
  const switchAccount = useMutation({
    mutationFn: () => api<void>("/api/auth/logout", { method: "POST", body: {} }),
    onSettled: () => {
      setCsrfToken(null);
      queryClient.removeQueries({ queryKey: ["oauth"] });
      // Stays on this address: the login page shows up here and comes back to the question.
      queryClient.setQueryData(["session"], null);
    },
  });

  const data = request.data;
  const redirect = data && "redirect" in data ? data.redirect : null;
  const ask = data && !("redirect" in data) ? data : null;
  const { mutate } = decide;

  // The app gets its answer from the server, even an error: that is the app's business then. The
  // server sends errors only to apps it trusts with them and refuses on the page otherwise.
  useEffect(() => {
    if (redirect) window.location.assign(redirect);
  }, [redirect]);

  // Allowed before with the same scopes: no question again, the app simply gets its code.
  const asked = useRef(false);
  const consented = ask?.consented ?? false;
  useEffect(() => {
    if (!consented || asked.current) return;
    asked.current = true;
    mutate(true);
  }, [consented, mutate]);

  // The session ran out in between: the login page takes over at this same address.
  const expired = loggedOut(request.error) || loggedOut(decide.error);
  useEffect(() => {
    if (!expired) return;
    queryClient.removeQueries({ queryKey: ["oauth"] });
    queryClient.setQueryData(["session"], null);
  }, [expired, queryClient]);

  const refused = refusal(request.error) ?? refusal(decide.error);
  if (refused) {
    return (
      <Frame>
        <EmptyState
          scene="loadError"
          title={t("oauth.invalidTitle")}
          body={t(`oauth.invalid.${refused}`)}
          action={
            <Button variant="primary" onClick={() => navigate("/account")}>
              {t("oauth.toPortal")}
            </Button>
          }
        />
      </Frame>
    );
  }
  if (request.isPending || expired) return <Loading fullPage />;
  if (request.isError) {
    return (
      <Frame>
        <LoadError error={request.error} onRetry={() => void request.refetch()} />
      </Frame>
    );
  }
  if (redirect || decide.isSuccess || (consented && !decide.isError) || !ask) return <Leaving />;

  const { client } = ask;
  const scopes = knownScopes(ask.scopes);
  return (
    <Frame>
      <div className="rounded-[22px] border border-hairline bg-surface px-6 pt-4 pb-7 shadow-float sm:px-8">
        <NyuScene name="addons" className="mx-auto h-auto w-[180px]" />
        <h1 className="mt-1 text-center text-[20px] font-bold break-words">
          {t("oauth.title", { name: client.name })}
        </h1>
        <p className="mt-1 text-center text-sm text-muted">
          {t("oauth.account")} <span className="font-semibold break-all text-ink">{session.account.login}</span>
        </p>
        <p className="mt-3 rounded-control bg-canvas px-3 py-2.5 text-[13px] text-muted">
          {t("oauth.nameClaim", { name: client.name })}{" "}
          {onThisDevice(client.redirectHost)
            ? t("oauth.returnsToDevice", { host: client.redirectHost })
            : t("oauth.returnsTo", { host: client.redirectHost })}
        </p>

        {scopes.length > 0 && (
          <>
            <h2 className="mt-5 text-[13px] font-semibold text-muted">{t("oauth.scopesTitle")}</h2>
            <ul className="mt-1 flex flex-col">
              {scopes.map((scope) => {
                const Icon = SCOPE_ICONS[scope];
                return (
                  <li key={scope} className="flex items-start gap-3 border-b border-hairline py-2.5 last:border-b-0">
                    <Icon className="mt-0.5 size-4 shrink-0 text-pink-ink" aria-hidden />
                    <span className="min-w-0 flex-1">
                      <span className="block text-sm font-semibold">{t(`oauth.scopes.${scope}.title`)}</span>
                      <span className="block text-[12px] text-muted">{t(`oauth.scopes.${scope}.hint`)}</span>
                    </span>
                  </li>
                );
              })}
            </ul>
          </>
        )}

        <p className="mt-4 text-[13px] text-faint">{t("oauth.revokeHint")}</p>
        {decide.isError && !expired && (
          <p role="alert" className="mt-3 text-center text-[13px] text-danger">
            {errorText(decide.error)}
          </p>
        )}
        <div className="mt-5 flex flex-col gap-2">
          <Button
            variant="primary"
            size="lg"
            icon={Check}
            busy={decide.isPending && decide.variables === true}
            disabled={decide.isPending}
            onClick={() => decide.mutate(true)}
          >
            {t("oauth.allow")}
          </Button>
          <Button
            size="lg"
            icon={X}
            busy={decide.isPending && decide.variables === false}
            disabled={decide.isPending}
            onClick={() => decide.mutate(false)}
          >
            {t("oauth.deny")}
          </Button>
        </div>
        <p className="mt-4 text-center text-[13px] text-muted">
          <button
            type="button"
            className="rounded-full font-semibold text-pink-ink hover:underline disabled:opacity-60"
            disabled={switchAccount.isPending || decide.isPending}
            onClick={() => switchAccount.mutate()}
          >
            {t("oauth.otherAccount", { login: session.account.login })}
          </button>
        </p>
      </div>
    </Frame>
  );
}
