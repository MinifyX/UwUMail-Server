import { ExternalLink, KeyRound, RotateCcw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/Button";
import { CopyButton } from "@/components/ui/Card";
import { useT } from "@/i18n";
import {
  api,
  ApiError,
  type FetchAccountInfo,
  type SignInProvider,
  type SignInStatus,
  type StartedSignIn,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { keepDraft, signInError, type SignInDraft } from "./signIn";

type Ready = Extract<SignInStatus, { status: "ready" }>;

/** The portal asks at least this often and at most this rarely; the server asks Microsoft per its interval. */
const MIN_POLL_MS = 1_000;
const MAX_POLL_MS = 10_000;

/**
 * Asks the server how a sign-in is doing until it is ready (proven with a real login) or failed.
 * The server asks Microsoft no more often than Microsoft wants, however often this asks.
 */
export function useSignInStatus(flowId: string | null, onReady: (ready: Ready) => void) {
  const [error, setError] = useState<string | null>(null);
  const ready = useRef(onReady);
  useEffect(() => {
    ready.current = onReady;
  }, [onReady]);

  useEffect(() => {
    if (!flowId) return;
    let stopped = false;
    let timer: number | undefined;
    const ask = async () => {
      try {
        const status = await api<SignInStatus>(`/api/account/fetch/oauth/flows/${encodeURIComponent(flowId)}`);
        if (stopped) return;
        if (status.status === "ready") {
          ready.current(status);
          return;
        }
        if (status.status === "failed") {
          setError(signInError(status.error));
          return;
        }
        const wait = Math.min(Math.max(status.retryIn * 1000, MIN_POLL_MS), MAX_POLL_MS);
        timer = window.setTimeout(() => void ask(), wait);
      } catch {
        // The server itself did not answer: ask again a little later rather than give up.
        if (!stopped) timer = window.setTimeout(() => void ask(), MAX_POLL_MS);
      }
    };
    timer = window.setTimeout(() => void ask(), MIN_POLL_MS);
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [flowId]);

  return { error, reset: () => setError(null) };
}

/** Saves a proven sign-in: a new mailbox with the dialog's choices, or an existing one switching to it. */
export async function saveSignIn(flowId: string, ready: Ready, draft: SignInDraft | null) {
  if (ready.switchId !== null) {
    return api<FetchAccountInfo>(`/api/account/fetch/${ready.switchId}`, {
      method: "PATCH",
      body: { oauthFlow: flowId },
    });
  }
  return api<FetchAccountInfo>("/api/account/fetch", {
    method: "POST",
    body: {
      address: ready.address,
      oauthFlow: flowId,
      ...(draft
        ? {
            afterFetch: draft.afterFetch,
            fetchJunk: draft.fetchJunk,
            intervalSecs: draft.intervalSecs,
            takeExisting: draft.takeExisting,
          }
        : {}),
    },
  });
}

/** The provider's name on its button, never translated. */
export function providerName(provider: SignInProvider): string {
  return provider === "microsoft" ? "Microsoft" : "Google";
}

/**
 * Signing in at Microsoft or Google for a fetched mailbox. Microsoft: a code to type at Microsoft's
 * page, which the server waits for. Google: off to Google's page and back to this one, which then
 * finishes it (see FinishSignInDialog). Once the sign-in is proven, `onSaved` gets the mailbox.
 */
export function SignInPanel({
  provider,
  address,
  switchId,
  draft,
  onSaved,
  autoStart = false,
  label,
}: {
  provider: SignInProvider;
  address: string;
  /** The mailbox that switches to signing in, when it is not a new one. */
  switchId?: number;
  /** What the dialog chose for a new mailbox. */
  draft?: SignInDraft;
  onSaved: (account: FetchAccountInfo) => void;
  /** Starts at once, for a mailbox whose sign-in ended and only has to be renewed. */
  autoStart?: boolean;
  /** The button's text, when it is not "Sign in with …". */
  label?: string;
}) {
  const { t } = useT();
  const errorText = useErrorText();
  const [device, setDevice] = useState<Extract<StartedSignIn, { provider: "microsoft" }>["device"] | null>(null);
  const [starting, setStarting] = useState(false);
  const [saving, setSaving] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  const status = useSignInStatus(device?.flowId ?? null, (ready) => {
    if (!device) return;
    setSaving(true);
    saveSignIn(device.flowId, ready, draft ?? null)
      .then(onSaved)
      .catch((err: unknown) => {
        setFailure(errorText(err));
        setDevice(null);
      })
      .finally(() => setSaving(false));
  });

  const start = async () => {
    setFailure(null);
    status.reset();
    setStarting(true);
    try {
      const started = await api<StartedSignIn>("/api/account/fetch/oauth/start", {
        method: "POST",
        body: { address: address.trim(), provider, ...(switchId !== undefined ? { switchId } : {}) },
      });
      if (started.provider === "microsoft") {
        setDevice(started.device);
      } else {
        // Off to Google; the way back lands on this page, which finishes the sign-in.
        if (draft) keepDraft(draft);
        window.location.assign(started.url);
      }
    } catch (err) {
      // The sign-in's own refusals have their own sentences; anything else is an API error.
      const code = err instanceof ApiError ? err.code : "";
      setFailure(signInError(code) === code ? t(`fetch.signIn.errors.${code}`) : errorText(err));
    } finally {
      setStarting(false);
    }
  };

  const autoStarted = useRef(false);
  useEffect(() => {
    if (autoStart && !autoStarted.current) {
      autoStarted.current = true;
      void start();
    }
    // Only once, when it opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoStart]);

  const failed = failure ?? (status.error ? t(`fetch.signIn.errors.${status.error}`) : null);
  const name = providerName(provider);

  if (device && !status.error) {
    return (
      <div className="flex flex-col gap-3 rounded-control border border-line bg-canvas p-4" aria-live="polite">
        <p className="text-[13px]">{t("fetch.signIn.deviceSteps")}</p>
        <div className="flex items-center justify-center gap-2 py-1">
          <code className="font-mono text-3xl font-bold tracking-[0.2em] select-all" data-testid="device-code">
            {device.userCode}
          </code>
          <CopyButton value={device.userCode} label={t("fetch.signIn.copyCode")} />
        </div>
        <a
          href={device.verificationUri}
          target="_blank"
          rel="noreferrer noopener"
          className="inline-flex items-center justify-center gap-1.5 self-center rounded-full bg-pink-solid px-4 py-2 text-sm font-semibold text-on-pink hover:bg-pink-solid-hover"
        >
          <ExternalLink className="size-4" aria-hidden />
          {t("fetch.signIn.openPage", { page: device.verificationUri.replace(/^https:\/\//, "") })}
        </a>
        <p className="flex items-center justify-center gap-2 text-[12px] text-muted">
          <span className="size-3 animate-spin rounded-full border-2 border-current border-t-transparent" aria-hidden />
          {saving ? t("fetch.signIn.saving") : t("fetch.signIn.waiting", { provider: name })}
        </p>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      <Button variant="primary" icon={failed ? RotateCcw : KeyRound} busy={starting} onClick={() => void start()}>
        {failed ? t("fetch.signIn.again") : (label ?? t("fetch.signIn.button", { provider: name }))}
      </Button>
      {failed && <p className="text-[13px] text-danger">{failed}</p>}
    </div>
  );
}
