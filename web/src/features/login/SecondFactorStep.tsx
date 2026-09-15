import { ArrowLeft, Fingerprint } from "lucide-react";
import { useState, type FormEvent } from "react";
import { NyuScene } from "@/components/nyu/scenes";
import { Button } from "@/components/ui/Button";
import { Field, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { ApiError, type SecondFactorChallenge } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { passkeysAvailable, wasCancelled } from "@/lib/webauthn";
import { usePasskeyLogin, useSecondFactorCode } from "@/features/session/session";

/** After the password: a code from the authenticator app, a recovery code, or a passkey. */
export function SecondFactorStep({
  challenge,
  hostname,
  onRestart,
}: {
  challenge: SecondFactorChallenge;
  hostname: string;
  onRestart: () => void;
}) {
  const { t } = useT();
  const errorText = useErrorText();
  const codeLogin = useSecondFactorCode();
  const passkeyLogin = usePasskeyLogin();
  const [recovery, setRecovery] = useState(!challenge.totp && !challenge.passkey);
  const [code, setCode] = useState("");
  const canUsePasskey = challenge.passkey && passkeysAvailable(hostname);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    passkeyLogin.reset();
    codeLogin.mutate({ token: challenge.token, code });
  };

  const failure =
    codeLogin.error ?? (passkeyLogin.error && !wasCancelled(passkeyLogin.error) ? passkeyLogin.error : null);
  const expired = failure instanceof ApiError && failure.code === "loginExpired";
  const showCodeForm = recovery || challenge.totp;

  return (
    <div className="flex flex-col">
      <NyuScene name="pick" className="mx-auto h-auto w-[170px]" />
      <h1 className="mt-1 text-center text-[20px] font-bold">{t("secondFactor.title")}</h1>
      <p className="mt-1 text-center text-[13px] text-muted">
        {recovery
          ? t("secondFactor.recoveryIntro")
          : challenge.totp
            ? t("secondFactor.totpIntro")
            : t("secondFactor.passkeyIntro")}
      </p>

      {expired ? (
        <div className="mt-5 flex flex-col gap-3">
          <p role="alert" className="text-center text-[13px] text-danger">
            {errorText(failure)}
          </p>
          <Button variant="primary" size="lg" onClick={onRestart}>
            {t("secondFactor.restart")}
          </Button>
        </div>
      ) : (
        <>
          {canUsePasskey && !recovery && (
            <Button
              variant={challenge.totp ? "secondary" : "primary"}
              size="lg"
              icon={Fingerprint}
              className="mt-5"
              busy={passkeyLogin.isPending}
              onClick={() => {
                codeLogin.reset();
                passkeyLogin.mutate(challenge.token);
              }}
            >
              {t("secondFactor.usePasskey")}
            </Button>
          )}
          {challenge.passkey && !challenge.totp && !canUsePasskey && !recovery && (
            <p className="mt-4 rounded-control bg-warning-tint px-3 py-2.5 text-[13px] text-warning">
              {t("secondFactor.passkeyHere", { hostname })}
            </p>
          )}

          {showCodeForm && (
            <form className="mt-5 flex flex-col gap-4" onSubmit={submit}>
              <Field
                label={recovery ? t("secondFactor.recoveryCode") : t("secondFactor.code")}
                error={failure && errorText(failure)}
              >
                {(id) => (
                  <TextInput
                    key={recovery ? "recovery" : "totp"}
                    id={id}
                    autoFocus
                    required
                    autoComplete={recovery ? "off" : "one-time-code"}
                    inputMode={recovery ? "text" : "numeric"}
                    autoCapitalize="none"
                    spellCheck={false}
                    placeholder={recovery ? "xxxxx-xxxxx" : "123456"}
                    className="text-center font-mono text-lg tracking-[0.2em]"
                    value={code}
                    onChange={(event) => setCode(event.target.value)}
                  />
                )}
              </Field>
              <Button type="submit" variant="primary" size="lg" busy={codeLogin.isPending}>
                {t("secondFactor.submit")}
              </Button>
            </form>
          )}
          {!showCodeForm && failure && (
            <p role="alert" className="mt-3 text-center text-[13px] text-danger">
              {errorText(failure)}
            </p>
          )}

          <div className="mt-4 flex flex-wrap items-center justify-between gap-2 text-[13px]">
            <button
              type="button"
              className="inline-flex items-center gap-1 rounded-full font-semibold text-muted hover:text-ink"
              onClick={onRestart}
            >
              <ArrowLeft className="size-3.5" aria-hidden />
              {t("secondFactor.back")}
            </button>
            {challenge.recoveryCodes && (challenge.totp || challenge.passkey) && (
              <button
                type="button"
                className="rounded-full font-semibold text-pink-ink hover:underline"
                onClick={() => {
                  setRecovery((value) => !value);
                  setCode("");
                  codeLogin.reset();
                  passkeyLogin.reset();
                }}
              >
                {recovery
                  ? challenge.totp
                    ? t("secondFactor.useApp")
                    : t("secondFactor.usePasskeyInstead")
                  : t("secondFactor.useRecovery")}
              </button>
            )}
          </div>
        </>
      )}
    </div>
  );
}
