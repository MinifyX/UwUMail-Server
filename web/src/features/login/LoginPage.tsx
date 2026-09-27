import { Eye, EyeOff, KeyRound } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import { NyuScene } from "@/components/nyu/scenes";
import { Button, IconButton } from "@/components/ui/Button";
import { Field, TextInput } from "@/components/ui/Field";
import { Wordmark } from "@/components/ui/Logo";
import { useT } from "@/i18n";
import { ApiError, needsSecondFactor } from "@/lib/api";
import { navigate } from "@/lib/router";
import { useInfo, useLogin } from "@/features/session/session";
import { loginNext, oidcError, oidcStartUrl, pendingLogin, withoutHandover } from "./external";
import { SecondFactorStep } from "./SecondFactorStep";

const KNOWN_ERRORS = ["invalidCredentials", "tooManyAttempts", "offline"];

export function LoginPage() {
  const { t } = useT();
  const info = useInfo();
  const login = useLogin();
  const [address, setAddress] = useState("");
  const [password, setPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  // What the server handed over after a login at the provider, read once when the page opens.
  const [providerError] = useState(() => oidcError(window.location.search));
  const [pending, setPending] = useState(() => pendingLogin(window.location.search));
  const [leaving, setLeaving] = useState(false);
  const setupRequired = info.data?.setupRequired;
  const oidc = info.data?.oidc;

  // Out of the address bar, so a reload does not show the error again or reuse the token.
  useEffect(() => {
    const { pathname, search } = window.location;
    const params = new URLSearchParams(search);
    if (params.has("oidcError") || params.has("pending") || params.has("methods")) {
      navigate(withoutHandover(pathname, search), { replace: true, scroll: false });
    }
  }, []);

  // Coming back from the provider with the browser's back button shows this page from its cache.
  useEffect(() => {
    const shown = (event: PageTransitionEvent) => {
      if (event.persisted) setLeaving(false);
    };
    window.addEventListener("pageshow", shown);
    return () => window.removeEventListener("pageshow", shown);
  }, []);

  // Without an admin nobody can log in yet: the setup assistant comes first.
  useEffect(() => {
    if (setupRequired) navigate("/setup", { replace: true });
  }, [setupRequired]);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    login.mutate({ login: address, password });
  };

  const errorCode = login.error instanceof ApiError ? login.error.code : login.error ? "internal" : null;
  const errorText = errorCode && t(`login.errors.${KNOWN_ERRORS.includes(errorCode) ? errorCode : "internal"}`);
  const challenge = login.data && needsSecondFactor(login.data) ? login.data.secondFactor : pending;
  const loginElsewhere = () => {
    setLeaving(true);
    window.location.assign(oidcStartUrl(loginNext(window.location.pathname, window.location.search)));
  };

  return (
    <main className="flex min-h-screen items-center justify-center px-4 py-10">
      <div className="w-full max-w-[420px] animate-slide-up">
        <div className="mb-6 flex justify-center">
          <Wordmark className="text-2xl" />
        </div>
        <div className="rounded-[22px] border border-hairline bg-surface px-6 pt-4 pb-7 shadow-float sm:px-8">
          {challenge ? (
            <SecondFactorStep
              key={challenge.token}
              challenge={challenge}
              hostname={info.data?.hostname ?? window.location.hostname}
              onRestart={() => {
                setPassword("");
                setPending(null);
                login.reset();
              }}
            />
          ) : (
            <>
              <NyuScene name="welcome" className="mx-auto h-auto w-[200px]" />
              <h1 className="mt-1 text-center text-[20px] font-bold">{t("login.title")}</h1>
              <p className="mt-1 text-center text-[13px] text-muted">{t("login.subtitle")}</p>
              {providerError && !login.error && (
                <p role="alert" className="mt-4 rounded-control bg-danger-tint px-3 py-2.5 text-[13px] text-danger">
                  {t(`login.oidcErrors.${providerError}`)}
                </p>
              )}

              <form className="mt-5 flex flex-col gap-4" onSubmit={submit}>
                <Field label={t("login.address")}>
                  {(id) => (
                    <TextInput
                      id={id}
                      type="email"
                      autoComplete="username"
                      autoFocus
                      required
                      value={address}
                      onChange={(event) => setAddress(event.target.value)}
                    />
                  )}
                </Field>
                <Field label={t("login.password")} error={errorText}>
                  {(id) => (
                    <span className="relative block">
                      <TextInput
                        id={id}
                        type={showPassword ? "text" : "password"}
                        autoComplete="current-password"
                        required
                        className="pr-12"
                        value={password}
                        onChange={(event) => setPassword(event.target.value)}
                      />
                      <IconButton
                        size="sm"
                        icon={showPassword ? EyeOff : Eye}
                        label={showPassword ? t("login.hidePassword") : t("login.showPassword")}
                        className="absolute top-1/2 right-1.5 -translate-y-1/2"
                        onClick={() => setShowPassword((value) => !value)}
                      />
                    </span>
                  )}
                </Field>
                <Button type="submit" variant="primary" size="lg" busy={login.isPending} className="mt-1">
                  {t("login.submit")}
                </Button>
              </form>
              {oidc && (
                <>
                  <p className="my-4 flex items-center gap-3 text-[12px] text-faint before:h-px before:flex-1 before:bg-hairline after:h-px after:flex-1 after:bg-hairline">
                    {t("login.or")}
                  </p>
                  <Button size="lg" icon={KeyRound} busy={leaving} className="w-full" onClick={loginElsewhere}>
                    {oidc.label ? t("login.oidcButton", { provider: oidc.label }) : t("login.oidcButtonGeneric")}
                  </Button>
                </>
              )}
            </>
          )}
        </div>
        {info.data && <p className="mt-5 text-center text-[12px] text-faint">{info.data.hostname}</p>}
      </div>
    </main>
  );
}
