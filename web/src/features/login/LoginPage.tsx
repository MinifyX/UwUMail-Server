import { Eye, EyeOff } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import { NyuScene } from "@/components/nyu/scenes";
import { Button, IconButton } from "@/components/ui/Button";
import { Field, TextInput } from "@/components/ui/Field";
import { Wordmark } from "@/components/ui/Logo";
import { useT } from "@/i18n";
import { ApiError, needsSecondFactor } from "@/lib/api";
import { navigate } from "@/lib/router";
import { useInfo, useLogin } from "@/features/session/session";
import { SecondFactorStep } from "./SecondFactorStep";

const KNOWN_ERRORS = ["invalidCredentials", "tooManyAttempts", "offline"];

export function LoginPage() {
  const { t } = useT();
  const info = useInfo();
  const login = useLogin();
  const [address, setAddress] = useState("");
  const [password, setPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  const setupRequired = info.data?.setupRequired;

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
  const challenge = login.data && needsSecondFactor(login.data) ? login.data.secondFactor : null;

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
                login.reset();
              }}
            />
          ) : (
            <>
              <NyuScene name="welcome" className="mx-auto h-auto w-[200px]" />
              <h1 className="mt-1 text-center text-[20px] font-bold">{t("login.title")}</h1>
              <p className="mt-1 text-center text-[13px] text-muted">{t("login.subtitle")}</p>

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
            </>
          )}
        </div>
        {info.data && <p className="mt-5 text-center text-[12px] text-faint">{info.data.hostname}</p>}
      </div>
    </main>
  );
}
