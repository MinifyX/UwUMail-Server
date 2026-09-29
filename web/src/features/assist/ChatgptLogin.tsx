import { CheckCircle2, ExternalLink, LogIn } from "lucide-react";
import { useEffect, useEffectEvent, useReducer } from "react";
import { Button } from "@/components/ui/Button";
import { CopyButton } from "@/components/ui/Card";
import { useT } from "@/i18n";
import { api } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatTime } from "@/lib/format";
import { loginReducer, type ChatgptLoginStart, type ChatgptPollAnswer } from "./model";

/**
 * Signs a ChatGPT provider in with OpenAI's device login: the server asks for a code, the person
 * enters it at OpenAI, and this asks the server every few seconds whether that happened.
 *
 * `prepare` stores a provider that is not saved yet and answers its id, or null when the form is
 * not ready; the login needs a provider to keep the tokens with.
 */
export function ChatgptLogin({
  basePath,
  providerId,
  connected,
  prepare,
  onConnected,
}: {
  basePath: string;
  providerId: number | null;
  connected: boolean;
  prepare: () => Promise<number | null>;
  onConnected: () => void;
}) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const [state, dispatch] = useReducer(loginReducer, { phase: "idle" });

  const start = async () => {
    dispatch({ type: "start" });
    try {
      const id = providerId ?? (await prepare());
      if (id === null) {
        dispatch({ type: "cancel" });
        return;
      }
      const answer = await api<ChatgptLoginStart>(`${basePath}/${id}/chatgpt/login`, { method: "POST" });
      dispatch({ type: "started", answer });
    } catch (failure) {
      dispatch({ type: "error", description: errorText(failure) });
    }
  };

  const poll = useEffectEvent(async (): Promise<ChatgptPollAnswer | null> => {
    if (providerId === null) return null;
    try {
      const answer = await api<ChatgptPollAnswer>(`${basePath}/${providerId}/chatgpt/poll`, { method: "POST" });
      if (answer.status === "connected") onConnected();
      return answer;
    } catch (failure) {
      dispatch({ type: "error", description: errorText(failure) });
      return null;
    }
  });

  // While a code is out: ask every `interval` seconds, until it is confirmed, runs out or fails.
  // A "pending" answer leaves the state as it is, so this loop keeps itself going.
  useEffect(() => {
    if (state.phase !== "waiting") return;
    let stopped = false;
    let timer = 0;
    const round = async () => {
      const now = Date.now() / 1000;
      if (now >= state.expiresAt) {
        dispatch({ type: "tick", now });
        return;
      }
      const answer = await poll();
      if (stopped || !answer) return;
      dispatch({ type: "polled", answer });
      if (answer.status === "pending") timer = window.setTimeout(() => void round(), state.interval * 1000);
    };
    timer = window.setTimeout(() => void round(), state.interval * 1000);
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [state]);

  if (state.phase === "waiting") {
    return (
      <div className="flex flex-col gap-3 rounded-control border border-line p-4">
        <p className="text-[13px] text-muted">{t("assist.chatgpt.step1")}</p>
        <div>
          <a
            href={state.verificationUri}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex h-10 items-center gap-2 rounded-full bg-pink-solid px-4 text-sm font-semibold text-on-pink hover:bg-pink-solid-hover"
          >
            <ExternalLink className="size-4" aria-hidden />
            {t("assist.chatgpt.open")}
          </a>
        </div>
        <p className="text-[13px] text-muted">{t("assist.chatgpt.step2")}</p>
        <div className="flex items-center gap-2">
          <output
            aria-label={t("assist.chatgpt.code")}
            className="rounded-control bg-canvas px-4 py-2 font-mono text-2xl font-bold tracking-[0.2em] select-all"
          >
            {state.userCode}
          </output>
          <CopyButton value={state.userCode} />
        </div>
        <p role="status" className="flex items-center gap-2 text-[13px] text-muted">
          <span
            className="size-3.5 animate-spin rounded-full border-2 border-current border-t-transparent"
            aria-hidden
          />
          {t("assist.chatgpt.waiting", { time: formatTime(state.expiresAt, i18n.language) })}
        </p>
        <div>
          <Button size="sm" variant="ghost" onClick={() => dispatch({ type: "cancel" })}>
            {t("common.cancel")}
          </Button>
        </div>
      </div>
    );
  }

  const done = state.phase === "connected" || (state.phase === "idle" && connected);
  return (
    <div className="flex flex-col gap-2">
      {done && (
        <p role="status" className="flex items-center gap-2 text-[13px] font-semibold text-success">
          <CheckCircle2 className="size-4" aria-hidden />
          {t("assist.chatgpt.connected")}
        </p>
      )}
      {state.phase === "expired" && <p className="text-[13px] text-danger">{t("assist.chatgpt.expired")}</p>}
      {state.phase === "failed" && (
        <p role="alert" className="text-[13px] break-words text-danger">
          {state.description
            ? t("assist.chatgpt.failed", { detail: state.description })
            : t("assist.chatgpt.failedPlain")}
        </p>
      )}
      <div>
        <Button
          icon={LogIn}
          variant={done ? "secondary" : "primary"}
          busy={state.phase === "starting"}
          onClick={() => void start()}
        >
          {done ? t("assist.chatgpt.again") : t("assist.chatgpt.signIn")}
        </Button>
      </div>
    </div>
  );
}
