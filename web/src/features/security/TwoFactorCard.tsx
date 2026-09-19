import { Fingerprint, KeyRound, ShieldCheck, Smartphone, Trash2 } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { ConfirmCloseSecretDialog } from "@/components/ui/ConfirmCloseSecretDialog";
import { ConfirmDiscardDialog } from "@/components/ui/ConfirmDiscardDialog";
import { Dialog } from "@/components/ui/Dialog";
import { Field, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type PasskeyInfo, type SecurityView, type Session, type TotpSetup } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDate, formatRelative } from "@/lib/format";
import { createPasskey, passkeysAvailable, wasCancelled, type CreationOptionsJson } from "@/lib/webauthn";
import { toast } from "@/state/toasts";
import { Cancelled } from "./ConfirmPassword";
import { useSecurityAction } from "./queries";
import { CopyTextButton, QrCode, RecoveryCodesBox } from "./SecurityBits";

type Confirmed = <T>(action: (password?: string) => Promise<T>) => Promise<T>;

/** Toasts real failures; closing the password dialog or the browser's passkey prompt is not one. */
function useFailure() {
  const errorText = useErrorText();
  return (error: unknown) => {
    if (error instanceof Cancelled || wasCancelled(error)) return;
    toast(errorText(error), "error");
  };
}

function CodesDialog({ codes, onClose }: { codes: string[] | null; onClose: () => void }) {
  const { t } = useT();
  // The codes are on screen once and nowhere else, so leaving is worth a question.
  const [confirming, setConfirming] = useState(false);
  return (
    <>
      <Dialog
        open={codes !== null}
        onClose={() => setConfirming(true)}
        closeOnOutsideClick={false}
        title={t("security.recovery.title")}
        width="sm"
      >
        <div className="flex flex-col gap-4 px-6 pt-1 pb-6">
          {codes && <RecoveryCodesBox codes={codes} />}
          <div className="flex justify-end">
            <Button variant="primary" onClick={onClose}>
              {t("security.recovery.saved")}
            </Button>
          </div>
        </div>
      </Dialog>
      <ConfirmCloseSecretDialog
        open={confirming}
        onBack={() => setConfirming(false)}
        onClose={() => {
          setConfirming(false);
          onClose();
        }}
      />
    </>
  );
}

function TotpSetupForm({
  setup,
  onDone,
  onDirtyChange,
}: {
  setup: TotpSetup;
  onDone: (codes: string[] | null) => void;
  onDirtyChange: (dirty: boolean) => void;
}) {
  const { t } = useT();
  const errorText = useErrorText();
  const [code, setCode] = useState("");
  const dirty = code.trim() !== "";
  useEffect(() => {
    onDirtyChange(dirty);
    return () => onDirtyChange(false);
  }, [dirty, onDirtyChange]);
  const confirm = useSecurityAction((value: string) =>
    api<{ recoveryCodes: string[] | null }>("/api/account/totp/confirm", { method: "POST", body: { code: value } }),
  );
  const submit = (event: FormEvent) => {
    event.preventDefault();
    confirm.mutate(code, {
      onSuccess: (result) => {
        toast(t("security.totp.enabled"), "success");
        onDone(result.recoveryCodes);
      },
    });
  };
  return (
    <form className="flex flex-col gap-4 px-6 pt-1 pb-6" onSubmit={submit}>
      <ol className="flex list-decimal flex-col gap-1 pl-5 text-sm text-muted">
        <li>{t("security.totp.step1")}</li>
        <li>{t("security.totp.step2")}</li>
        <li>{t("security.totp.step3")}</li>
      </ol>
      <div className="flex flex-col items-center gap-3 sm:flex-row sm:items-start">
        {setup.qr && <QrCode size={setup.qr.size} modules={setup.qr.modules} label={t("security.totp.qrLabel")} />}
        <div className="flex min-w-0 flex-col gap-2">
          <p className="text-[13px] text-muted">{t("security.totp.manual")}</p>
          <code className="rounded-control bg-canvas px-3 py-2 font-mono text-[13px] break-all select-all">
            {setup.secret.replace(/(.{4})/g, "$1 ").trim()}
          </code>
          <div>
            <CopyTextButton value={setup.secret} label={t("security.totp.copySecret")} />
          </div>
        </div>
      </div>
      <Field label={t("security.totp.code")} error={confirm.isError ? errorText(confirm.error) : undefined}>
        {(id) => (
          <TextInput
            id={id}
            required
            autoComplete="one-time-code"
            inputMode="numeric"
            placeholder="123456"
            className="font-mono tracking-[0.2em]"
            value={code}
            onChange={(event) => setCode(event.target.value)}
          />
        )}
      </Field>
      <div className="flex justify-end">
        <Button type="submit" variant="primary" busy={confirm.isPending}>
          {t("security.totp.activate")}
        </Button>
      </div>
    </form>
  );
}

function PasskeyRow({ passkey, onRemove }: { passkey: PasskeyInfo; onRemove: () => void }) {
  const { t, i18n } = useT();
  return (
    <li className="flex min-h-12 items-center gap-3 border-b border-hairline py-2 last:border-b-0">
      <Fingerprint className="size-4 shrink-0 text-muted" aria-hidden />
      <span className="min-w-0 flex-1">
        <span className="block truncate text-sm font-semibold">{passkey.name}</span>
        <span className="block text-[12px] text-muted">
          {passkey.lastUsedAt
            ? t("security.passkeys.lastUsed", { time: formatRelative(passkey.lastUsedAt, i18n.language) })
            : t("security.passkeys.added", { date: formatDate(passkey.createdAt, i18n.language) })}
        </span>
      </span>
      <Button size="sm" variant="danger" icon={Trash2} onClick={onRemove}>
        {t("security.passkeys.remove")}
      </Button>
    </li>
  );
}

export function TwoFactorCard({
  security,
  session,
  confirmed,
}: {
  security: SecurityView;
  session: Session;
  confirmed: Confirmed;
}) {
  const { t } = useT();
  const failure = useFailure();
  const [setup, setSetup] = useState<TotpSetup | null>(null);
  const [codes, setCodes] = useState<string[] | null>(null);
  const [passkeyName, setPasskeyName] = useState<string | null>(null);
  const canAddPasskey = passkeysAvailable(session.server.hostname);
  const [totpDirty, setTotpDirty] = useState(false);
  const [discarding, setDiscarding] = useState<"totp" | "passkey" | null>(null);

  const passkeyDirty = (passkeyName ?? "").trim() !== "";
  const requestCloseTotp = () => (totpDirty ? setDiscarding("totp") : setSetup(null));
  const requestClosePasskey = () => (passkeyDirty ? setDiscarding("passkey") : setPasskeyName(null));

  const startTotp = useSecurityAction(() =>
    confirmed((password) => api<TotpSetup>("/api/account/totp", { method: "POST", body: { password } })),
  );
  const disableTotp = useSecurityAction(() =>
    confirmed((password) => api<void>("/api/account/totp", { method: "DELETE", body: { password } })),
  );
  const newCodes = useSecurityAction(() =>
    confirmed((password) =>
      api<{ recoveryCodes: string[] }>("/api/account/recovery-codes", { method: "POST", body: { password } }),
    ),
  );
  const addPasskey = useSecurityAction(async (name: string) => {
    const options = await confirmed((password) =>
      api<CreationOptionsJson>("/api/account/passkeys/options", { method: "POST", body: { password } }),
    );
    const credential = await createPasskey(options);
    return api<{ recoveryCodes: string[] | null }>("/api/account/passkeys", {
      method: "POST",
      body: { name, credential },
    });
  });
  const removePasskey = useSecurityAction((id: number) =>
    confirmed((password) => api<void>(`/api/account/passkeys/${id}`, { method: "DELETE", body: { password } })),
  );

  return (
    <Card title={t("security.twoFactor.title")}>
      <div className="flex flex-col gap-5">
        <p className="flex items-start gap-2 text-sm">
          <ShieldCheck
            className={
              security.secondFactor ? "mt-0.5 size-4 shrink-0 text-success" : "mt-0.5 size-4 shrink-0 text-muted"
            }
            aria-hidden
          />
          <span className={security.secondFactor ? "" : "text-muted"}>
            {security.secondFactor ? t("security.twoFactor.on") : t("security.twoFactor.off")}
          </span>
        </p>

        <section className="flex flex-col gap-2">
          <h3 className="flex items-center gap-2 text-[13px] font-bold">
            <Smartphone className="size-4 text-muted" aria-hidden />
            {t("security.totp.title")}
          </h3>
          {security.totp ? (
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="text-sm text-muted">{t("security.totp.active")}</p>
              <Button
                size="sm"
                variant="danger"
                busy={disableTotp.isPending}
                onClick={() =>
                  disableTotp.mutate(undefined, {
                    onSuccess: () => toast(t("security.totp.disabled"), "success"),
                    onError: failure,
                  })
                }
              >
                {t("security.totp.disable")}
              </Button>
            </div>
          ) : (
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="text-sm text-muted">{t("security.totp.inactive")}</p>
              <Button
                size="sm"
                variant="primary"
                busy={startTotp.isPending}
                onClick={() => startTotp.mutate(undefined, { onSuccess: setSetup, onError: failure })}
              >
                {t("security.totp.setUp")}
              </Button>
            </div>
          )}
        </section>

        <section className="flex flex-col gap-2 border-t border-hairline pt-4">
          <h3 className="flex items-center gap-2 text-[13px] font-bold">
            <Fingerprint className="size-4 text-muted" aria-hidden />
            {t("security.passkeys.title")}
          </h3>
          {security.passkeys.length > 0 && (
            <ul className="flex flex-col">
              {security.passkeys.map((passkey) => (
                <PasskeyRow
                  key={passkey.id}
                  passkey={passkey}
                  onRemove={() =>
                    removePasskey.mutate(passkey.id, {
                      onSuccess: () => toast(t("security.passkeys.removed", { name: passkey.name }), "success"),
                      onError: failure,
                    })
                  }
                />
              ))}
            </ul>
          )}
          {canAddPasskey ? (
            <div>
              <Button size="sm" icon={Fingerprint} onClick={() => setPasskeyName("")}>
                {t("security.passkeys.add")}
              </Button>
            </div>
          ) : (
            <p className="text-[13px] text-muted">
              {t("security.passkeys.unavailable", { hostname: session.server.hostname })}
            </p>
          )}
        </section>

        {security.secondFactor && (
          <section className="flex flex-col gap-2 border-t border-hairline pt-4">
            <h3 className="flex items-center gap-2 text-[13px] font-bold">
              <KeyRound className="size-4 text-muted" aria-hidden />
              {t("security.recovery.title")}
            </h3>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className={security.recoveryCodesLeft <= 2 ? "text-sm text-warning" : "text-sm text-muted"}>
                {t("security.recovery.left", { count: security.recoveryCodesLeft })}
              </p>
              <Button
                size="sm"
                busy={newCodes.isPending}
                onClick={() =>
                  newCodes.mutate(undefined, {
                    onSuccess: (result) => setCodes(result.recoveryCodes),
                    onError: failure,
                  })
                }
              >
                {t("security.recovery.renew")}
              </Button>
            </div>
          </section>
        )}
      </div>

      <Dialog
        open={setup !== null}
        onClose={requestCloseTotp}
        closeOnOutsideClick={!totpDirty}
        title={t("security.totp.setUpTitle")}
      >
        {setup && (
          <TotpSetupForm
            setup={setup}
            onDirtyChange={setTotpDirty}
            onDone={(recoveryCodes) => {
              setSetup(null);
              if (recoveryCodes) setCodes(recoveryCodes);
            }}
          />
        )}
      </Dialog>

      <Dialog
        open={passkeyName !== null}
        onClose={requestClosePasskey}
        closeOnOutsideClick={!passkeyDirty}
        title={t("security.passkeys.addTitle")}
        width="sm"
      >
        {passkeyName !== null && (
          <form
            className="flex flex-col gap-4 px-6 pt-1 pb-6"
            onSubmit={(event) => {
              event.preventDefault();
              addPasskey.mutate(passkeyName, {
                onSuccess: (result) => {
                  setPasskeyName(null);
                  toast(t("security.passkeys.addedToast"), "success");
                  if (result.recoveryCodes) setCodes(result.recoveryCodes);
                },
                onError: failure,
              });
            }}
          >
            <p className="text-sm text-muted">{t("security.passkeys.addBody")}</p>
            <Field label={t("security.passkeys.name")} hint={t("security.passkeys.nameHint")}>
              {(id) => (
                <TextInput
                  id={id}
                  autoFocus
                  maxLength={60}
                  value={passkeyName}
                  onChange={(event) => setPasskeyName(event.target.value)}
                />
              )}
            </Field>
            <div className="flex justify-end gap-2">
              <Button onClick={requestClosePasskey}>{t("common.cancel")}</Button>
              <Button type="submit" variant="primary" icon={Fingerprint} busy={addPasskey.isPending}>
                {t("security.passkeys.create")}
              </Button>
            </div>
          </form>
        )}
      </Dialog>

      <CodesDialog codes={codes} onClose={() => setCodes(null)} />

      <ConfirmDiscardDialog
        open={discarding !== null}
        onKeepEditing={() => setDiscarding(null)}
        onDiscard={() => {
          if (discarding === "totp") setSetup(null);
          else setPasskeyName(null);
          setDiscarding(null);
        }}
      />
    </Card>
  );
}
