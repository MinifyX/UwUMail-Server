import clsx from "clsx";
import { useT } from "@/i18n";
import type { MoveMailboxState, MoveState } from "@/lib/api";

const MOVE_STYLES: Record<MoveState, string> = {
  active: "bg-pink-tint text-pink-ink",
  paused: "bg-warning-tint text-warning",
  finishing: "bg-pink-tint text-pink-ink",
  done: "bg-success-tint text-success",
};

const MAILBOX_STYLES: Record<MoveMailboxState, string> = {
  queued: "border border-line text-muted",
  running: "bg-pink-tint text-pink-ink",
  paused: "bg-warning-tint text-warning",
  synced: "bg-success-tint text-success",
  done: "bg-success-tint text-success",
};

const pill = "inline-flex h-6 shrink-0 items-center rounded-full px-2.5 text-[12px] font-semibold";

export function MoveStatePill({ state }: { state: MoveState }) {
  const { t } = useT();
  return <span className={clsx(pill, MOVE_STYLES[state])}>{t(`moves.state.${state}`)}</span>;
}

export function MailboxStatePill({ state }: { state: MoveMailboxState }) {
  const { t } = useT();
  return <span className={clsx(pill, MAILBOX_STYLES[state])}>{t(`moves.mailboxState.${state}`)}</span>;
}

export function ProgressBar({ value, label }: { value: number; label: string }) {
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(value * 100)}
      className="h-2 overflow-hidden rounded-full bg-canvas"
    >
      <div className="h-full rounded-full bg-pink transition-[width]" style={{ width: `${value * 100}%` }} />
    </div>
  );
}
