import { useEffect, useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { ConfirmDiscardDialog } from "@/components/ui/ConfirmDiscardDialog";
import { Dialog } from "@/components/ui/Dialog";
import { Field, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { ApiError } from "@/lib/api";
import { useErrorText } from "@/lib/errors";

/** Thrown when someone closes the password dialog instead of confirming. */
export class Cancelled extends Error {
  constructor() {
    super("cancelled");
    this.name = "Cancelled";
  }
}

interface Pending {
  run: (password: string) => Promise<unknown>;
  resolve: (value: unknown) => void;
  reject: (error: unknown) => void;
}

function PasswordForm({
  pending,
  onClose,
  onCancel,
  onDirtyChange,
}: {
  pending: Pending;
  onClose: () => void;
  onCancel: () => void;
  onDirtyChange: (dirty: boolean) => void;
}) {
  const { t } = useT();
  const errorText = useErrorText();
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const dirty = password !== "";
  useEffect(() => {
    onDirtyChange(dirty);
    return () => onDirtyChange(false);
  }, [dirty, onDirtyChange]);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      pending.resolve(await pending.run(password));
      onClose();
    } catch (failure) {
      if (failure instanceof ApiError && (failure.code === "wrongPassword" || failure.code === "tooManyAttempts")) {
        setError(failure);
      } else {
        pending.reject(failure);
        onClose();
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="flex flex-col gap-4 px-6 pt-1 pb-6" onSubmit={(event) => void submit(event)}>
      <p className="text-sm text-muted">{t("security.confirm.body")}</p>
      <Field label={t("security.confirm.password")} error={error ? errorText(error) : undefined}>
        {(id) => (
          <TextInput
            id={id}
            type="password"
            autoComplete="current-password"
            autoFocus
            required
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
        )}
      </Field>
      <div className="flex justify-end gap-2">
        <Button onClick={onCancel}>{t("common.cancel")}</Button>
        <Button type="submit" variant="primary" busy={busy}>
          {t("security.confirm.submit")}
        </Button>
      </div>
    </form>
  );
}

/**
 * Sensitive changes need the password again unless the login is fresh. `confirmed` runs an action;
 * when the server asks for the password, a dialog asks for it and runs the action once more with it.
 */
export function usePasswordConfirmation() {
  const { t } = useT();
  const [pending, setPending] = useState<Pending | null>(null);
  const [dirty, setDirty] = useState(false);
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);

  const confirmed = async <T,>(action: (password?: string) => Promise<T>): Promise<T> => {
    try {
      return await action();
    } catch (error) {
      if (!(error instanceof ApiError && error.code === "confirmPassword")) throw error;
      return new Promise<T>((resolve, reject) => {
        setPending({ run: action, resolve: resolve as (value: unknown) => void, reject });
      });
    }
  };

  const close = () => setPending(null);
  const cancel = () => {
    pending?.reject(new Cancelled());
    close();
  };
  const requestCancel = () => {
    if (dirty) setConfirmingDiscard(true);
    else cancel();
  };
  const dialog = (
    <>
      <Dialog
        open={pending !== null}
        onClose={requestCancel}
        dismissable={!dirty}
        title={t("security.confirm.title")}
        width="sm"
      >
        {pending && (
          <PasswordForm pending={pending} onClose={close} onCancel={requestCancel} onDirtyChange={setDirty} />
        )}
      </Dialog>
      <ConfirmDiscardDialog
        open={confirmingDiscard}
        onKeepEditing={() => setConfirmingDiscard(false)}
        onDiscard={() => {
          setConfirmingDiscard(false);
          cancel();
        }}
      />
    </>
  );
  return { confirmed, dialog };
}
