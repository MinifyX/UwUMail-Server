import clsx from "clsx";
import { ChevronDown } from "lucide-react";
import { forwardRef, useId, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes } from "react";

interface FieldProps {
  label: ReactNode;
  hint?: ReactNode;
  error?: ReactNode;
  children: (id: string) => ReactNode;
  className?: string;
}

export function Field({ label, hint, error, children, className }: FieldProps) {
  const id = useId();
  return (
    <div className={clsx("flex flex-col gap-1.5", className)}>
      <label htmlFor={id} className="text-[13px] font-semibold text-muted">
        {label}
      </label>
      {children(id)}
      {error ? (
        <p role="alert" className="text-[13px] text-danger">
          {error}
        </p>
      ) : (
        hint && <p className="text-[12px] text-muted">{hint}</p>
      )}
    </div>
  );
}

const controlClass =
  "h-11 w-full rounded-control border border-control bg-surface px-3.5 text-sm text-ink placeholder:text-muted transition-shadow focus:border-pink focus:shadow-focus focus:outline-none";

export const TextInput = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function TextInput(
  { className, ...rest },
  ref,
) {
  return <input ref={ref} className={clsx(controlClass, className)} {...rest} />;
});

export function Select({ className, children, ...rest }: SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <span className={clsx("relative block", className)}>
      <select className={clsx(controlClass, "appearance-none pr-10")} {...rest}>
        {children}
      </select>
      <ChevronDown
        className="pointer-events-none absolute top-1/2 right-3.5 size-4 -translate-y-1/2 text-muted"
        aria-hidden
      />
    </span>
  );
}

interface ToggleProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
}

export function Toggle({ checked, onChange, label, description, disabled }: ToggleProps) {
  return (
    <SettingRow label={label} description={description} disabled={disabled} wrap={false}>
      {({ id, describedBy }) => (
        <button
          id={id}
          type="button"
          role="switch"
          aria-checked={checked}
          aria-describedby={describedBy}
          disabled={disabled}
          onClick={() => onChange(!checked)}
          className={clsx(
            "relative h-6 w-11 shrink-0 rounded-full border transition-colors duration-200 disabled:cursor-not-allowed",
            checked ? "border-pink-solid bg-pink-solid" : "border-control bg-surface",
          )}
        >
          <span
            className={clsx(
              "absolute top-px left-px size-5 rounded-full shadow transition-transform duration-200",
              checked ? "translate-x-5 bg-on-pink" : "bg-control",
            )}
          />
        </button>
      )}
    </SettingRow>
  );
}

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
 * One setting: what it is and what it does on the left, the control on the right. Where the row
 * gets too narrow, the control moves below the text instead of squeezing it.
 */
export function SettingRow({ label, description, children, disabled, wrap = true, className }: SettingRowProps) {
  const id = useId();
  const descriptionId = description ? `${id}-description` : undefined;
  const labelled = typeof children === "function";
  return (
    <div
      className={clsx(
        "flex items-center justify-between gap-x-6 gap-y-2.5",
        wrap && "flex-wrap",
        disabled && "opacity-60",
        className,
      )}
    >
      <div className={clsx("flex flex-1 flex-col gap-0.5", wrap && "min-w-[min(220px,100%)]")}>
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
      <div className={clsx("flex max-w-full", wrap ? "shrink-0" : "shrink-0 self-start pt-0.5")}>
        {typeof children === "function" ? children({ id, describedBy: descriptionId }) : children}
      </div>
    </div>
  );
}

interface SegmentedProps<T extends string> {
  value: T;
  onChange: (value: T) => void;
  options: { value: T; label: ReactNode }[];
  label: string;
  /** Ties the group to a visible description, e.g. that of a SettingRow. */
  describedBy?: string;
  disabled?: boolean;
}

export function Segmented<T extends string>({
  value,
  onChange,
  options,
  label,
  describedBy,
  disabled,
}: SegmentedProps<T>) {
  return (
    <div
      role="radiogroup"
      aria-label={label}
      aria-describedby={describedBy}
      className="inline-flex max-w-full [scrollbar-width:none] self-start overflow-x-auto rounded-full bg-canvas p-1"
    >
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={value === option.value}
          disabled={disabled}
          onClick={() => onChange(option.value)}
          className={clsx(
            "h-8 shrink-0 rounded-full px-4 text-[13px] font-semibold whitespace-nowrap transition-colors disabled:cursor-not-allowed disabled:opacity-60",
            value === option.value ? "bg-surface text-pink-ink shadow-sm" : "text-muted hover:text-ink",
          )}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}
