import clsx from "clsx";
import type { LucideIcon } from "lucide-react";
import { forwardRef, type ButtonHTMLAttributes, type ReactNode } from "react";

type Variant = "primary" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md" | "lg";

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  icon?: LucideIcon;
  busy?: boolean;
  children?: ReactNode;
}

const VARIANTS: Record<Variant, string> = {
  primary: "bg-pink-solid text-on-pink hover:bg-pink-solid-hover shadow-[0_2px_10px_rgb(225_29_116/0.25)]",
  secondary: "bg-surface text-ink border border-line hover:bg-elevated hover:border-faint/50",
  ghost: "text-ink hover:bg-pink-tint/60",
  danger: "bg-surface text-danger border border-line hover:bg-danger-tint",
};

const SIZES: Record<Size, string> = {
  sm: "h-8 px-3 text-[13px] gap-1.5",
  md: "h-10 px-4 text-sm gap-2",
  lg: "h-12 px-6 text-[15px] gap-2",
};

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "secondary", size = "md", icon: Icon, busy, className, children, disabled, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type="button"
      className={clsx(
        "inline-flex shrink-0 items-center justify-center rounded-full font-semibold whitespace-nowrap transition-[background,box-shadow,transform] duration-150 active:scale-[0.97] disabled:pointer-events-none disabled:opacity-55",
        VARIANTS[variant],
        SIZES[size],
        className,
      )}
      disabled={disabled || busy}
      aria-busy={busy || undefined}
      {...rest}
    >
      {busy ? (
        <span className="size-4 animate-spin rounded-full border-2 border-current border-t-transparent" aria-hidden />
      ) : (
        Icon && <Icon className="size-4" strokeWidth={2.2} aria-hidden />
      )}
      {children}
    </button>
  );
});

interface IconButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  icon: LucideIcon;
  label: string;
  active?: boolean;
  size?: "sm" | "md";
}

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  { icon: Icon, label, active, size = "md", className, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type="button"
      aria-label={label}
      title={label}
      className={clsx(
        "inline-flex shrink-0 items-center justify-center rounded-full transition-colors duration-150 disabled:opacity-40",
        size === "md" ? "size-9" : "size-8",
        active ? "bg-pink-tint text-pink-ink" : "text-muted hover:bg-pink-tint/60 hover:text-ink",
        className,
      )}
      {...rest}
    >
      <Icon className={size === "md" ? "size-[18px]" : "size-4"} strokeWidth={2} aria-hidden />
    </button>
  );
});
