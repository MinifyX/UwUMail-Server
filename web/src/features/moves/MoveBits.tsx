import { cx } from "@uwusuite/design";
import { useT } from "@/i18n";
import type { MoveMailboxState, MoveState } from "@/lib/api";

const MOVE_STYLES: Record<MoveState, string> = {
  active: "bg-pink-tint text-pink-ink",
  paused: "bg-warning-tint text-warning-ink",
  finishing: "bg-pink-tint text-pink-ink",
  done: "bg-success-tint text-success-ink",
};

const MAILBOX_STYLES: Record<MoveMailboxState, string> = {
  queued: "border border-line text-muted",
  running: "bg-pink-tint text-pink-ink",
  paused: "bg-warning-tint text-warning-ink",
  synced: "bg-success-tint text-success-ink",
  done: "bg-success-tint text-success-ink",
};

const pill = "inline-flex h-6 shrink-0 items-center rounded-full px-2.5 text-[12px] font-semibold";

export function MoveStatePill({ state }: { state: MoveState }) {
  const { t } = useT();
  return <span className={cx(pill, MOVE_STYLES[state])}>{t(`moves.state.${state}`)}</span>;
}

export function MailboxStatePill({ state }: { state: MoveMailboxState }) {
  const { t } = useT();
  return <span className={cx(pill, MAILBOX_STYLES[state])}>{t(`moves.mailboxState.${state}`)}</span>;
}
