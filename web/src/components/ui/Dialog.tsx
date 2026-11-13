import { X } from "lucide-react";
import { useEffect, useId, useRef, type ReactNode } from "react";
import { useT } from "@/i18n";
import { cx, IconButton } from "@uwusuite/design";

interface DialogProps {
  open: boolean;
  onClose: () => void;
  title?: ReactNode;
  children: ReactNode;
  width?: "sm" | "md" | "lg" | "viewer";
  className?: string;
  /** The id of the heading that names a dialog drawn without `title`. */
  labelledBy?: string;
}

/**
 * Modal built on <dialog>: focus trapping, Escape and backdrop come from the browser. It closes
 * only on purpose (the X, Escape, a button inside), never on a click beside it.
 *
 * shortcut: a copy of @uwusuite/design's Dialog because that one does not tie the dialog to its
 * title (aria-labelledby); switch to the package once it does.
 */
export function Dialog({ open, onClose, title, children, width = "md", className, labelledBy }: DialogProps) {
  const ref = useRef<HTMLDialogElement>(null);
  const { t } = useT();
  const titleId = useId();

  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  }, [open]);

  return (
    <dialog
      ref={ref}
      aria-labelledby={title !== undefined ? titleId : labelledBy}
      onCancel={(event) => {
        // Escape in a nested dialog closes only that one.
        if (event.target !== ref.current) return;
        event.preventDefault();
        onClose();
      }}
      className={cx(
        // svh, not vh: on a phone `vh` is the height with the address bar hidden, so a dialog sized
        // by it reaches below the visible area and takes its buttons with it.
        "m-auto max-h-[min(720px,calc(100svh-48px))] w-[calc(100vw-48px)] overflow-hidden rounded-dialog border border-line bg-surface p-0 text-ink shadow-float backdrop:bg-backdrop backdrop:backdrop-blur-[2px] open:flex open:animate-pop open:flex-col",
        width === "sm" && "max-w-[420px]",
        width === "md" && "max-w-[560px]",
        width === "lg" && "max-w-[860px]",
        width === "viewer" && "h-[calc(100svh-48px)] max-h-none max-w-[1200px]",
        // Phones: everything but small confirmations fills the screen.
        width !== "sm" &&
          "phone:h-full phone:max-h-none phone:w-full phone:max-w-none phone:rounded-none phone:border-0 phone:pt-[env(safe-area-inset-top)] phone:pb-[env(safe-area-inset-bottom)]",
        className,
      )}
    >
      {/* Safari sizes a <dialog> as fit-content, and in WebKit that is 0 for a column whose items
          have flex-basis 0 (flex-1) or a percentage height. So items start from their content
          (flex-auto) and shrink from there (min-h-0). Never flex-1 or h-full in here. */}
      {open && (
        <div className="flex min-h-0 flex-auto flex-col">
          {title !== undefined && (
            <header className="flex items-center justify-between gap-4 px-6 pt-5 pb-2">
              <h2 id={titleId} className="text-section font-bold">
                {title}
              </h2>
              <IconButton icon={X} label={t("common.close")} onClick={onClose} />
            </header>
          )}
          <div className="min-h-0 flex-auto overflow-y-auto">{children}</div>
        </div>
      )}
    </dialog>
  );
}
