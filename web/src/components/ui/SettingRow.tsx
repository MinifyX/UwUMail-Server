import { cx } from "@uwusuite/design";
import { useId, type ReactNode } from "react";

interface SettingRowProps {
  label: ReactNode;
  description?: ReactNode;
  /** The control; as a function it gets the ids that tie it to the label and the description. */
  children: ReactNode | ((ids: { id: string; describedBy?: string }) => ReactNode);
  disabled?: boolean;
  /** Whether the control may move below the text on a narrow screen (a switch stays beside it). */
  wrap?: boolean;
  className?: string;
}

/**
 * Kept here: the package SettingRow takes only a ready control and draws a hairline under each row;
 * this one hands the control the ids of its label and description and can show a disabled row.
 *
 * One setting: what it is and what it does on the left, the control on the right. Where the row
 * gets too narrow, the control moves below the text instead of squeezing it.
 */
export function SettingRow({ label, description, children, disabled, wrap = true, className }: SettingRowProps) {
  const id = useId();
  const descriptionId = description ? `${id}-description` : undefined;
  const labelled = typeof children === "function";
  return (
    <div
      className={cx(
        "flex items-center justify-between gap-x-6 gap-y-2.5",
        wrap && "flex-wrap",
        disabled && "opacity-60",
        className,
      )}
    >
      <div className={cx("flex flex-1 flex-col gap-0.5", wrap && "min-w-[min(220px,100%)]")}>
        {labelled ? (
          <label htmlFor={id} className="text-sm font-semibold">
            {label}
          </label>
        ) : (
          <span id={id} className="text-sm font-semibold">
            {label}
          </span>
        )}
        {description && (
          <span id={descriptionId} className="text-[13px] text-muted">
            {description}
          </span>
        )}
      </div>
      <div className={cx("flex max-w-full", wrap ? "shrink-0" : "shrink-0 self-start pt-0.5")}>
        {typeof children === "function" ? children({ id, describedBy: descriptionId }) : children}
      </div>
    </div>
  );
}
