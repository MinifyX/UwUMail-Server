import { createToasts, type ToastTone } from "@uwusuite/design";

/** The portal's toasts, shown by the package Toaster in App. */
export const toasts = createToasts();

/** Shows a toast and returns its id, e.g. to dismiss it early. */
export const toast = (message: string, tone: ToastTone = "info") => toasts.show(message, { tone });
