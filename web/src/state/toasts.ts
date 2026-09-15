import { create } from "zustand";

export type ToastTone = "info" | "success" | "error";

/** A little flourish on top of the message. "sent" lets Nyu fly off with the mail. */
export type ToastEffect = "sent";

export interface ToastAction {
  label: string;
  run: () => void;
}

export interface ToastOptions {
  action?: ToastAction;
  /** Milliseconds until it goes away by itself. */
  duration?: number;
}

export interface Toast {
  id: number;
  message: string;
  tone: ToastTone;
  effect?: ToastEffect;
  action?: ToastAction;
}

interface ToastState {
  toasts: Toast[];
  show: (message: string, tone?: ToastTone, effect?: ToastEffect, options?: ToastOptions) => number;
  dismiss: (id: number) => void;
}

let nextId = 1;

export const useToasts = create<ToastState>()((set, get) => ({
  toasts: [],
  show: (message, tone = "info", effect, options = {}) => {
    const id = nextId++;
    set({ toasts: [...get().toasts.slice(-3), { id, message, tone, effect, action: options.action }] });
    setTimeout(() => get().dismiss(id), options.duration ?? (tone === "error" ? 7000 : 3500));
    return id;
  },
  dismiss: (id) => set({ toasts: get().toasts.filter((t) => t.id !== id) }),
}));

/** Shows a toast and returns its id, e.g. to dismiss it early. */
export const toast = (message: string, tone?: ToastTone, effect?: ToastEffect, options?: ToastOptions) =>
  useToasts.getState().show(message, tone, effect, options);
