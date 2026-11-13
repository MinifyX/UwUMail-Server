import { cx } from "@uwusuite/design";

/**
 * A state as a dot and a word: never colour alone. Kept here: the package StatusDot has no
 * danger state (its states are online, connecting, offline and error).
 */
export function StatusDot({
  tone,
  className,
}: {
  tone: "success" | "warning" | "danger" | "offline";
  className?: string;
}) {
  const color = { success: "bg-success", warning: "bg-warning", danger: "bg-danger", offline: "bg-offline" }[tone];
  return <span className={cx("inline-block size-2 shrink-0 rounded-full", color, className)} aria-hidden />;
}
