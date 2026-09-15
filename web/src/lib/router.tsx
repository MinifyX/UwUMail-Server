import type { AnchorHTMLAttributes, MouseEvent } from "react";
import { useSyncExternalStore } from "react";

/** A tiny router over the History API: the portal has few pages and no nested data loading. */

const NAVIGATE_EVENT = "uwumail:navigate";

function subscribe(callback: () => void) {
  window.addEventListener("popstate", callback);
  window.addEventListener(NAVIGATE_EVENT, callback);
  return () => {
    window.removeEventListener("popstate", callback);
    window.removeEventListener(NAVIGATE_EVENT, callback);
  };
}

export function usePath(): string {
  return useSyncExternalStore(subscribe, () => window.location.pathname);
}

export function navigate(to: string, { replace = false } = {}) {
  if (to === window.location.pathname + window.location.search) return;
  if (replace) window.history.replaceState(null, "", to);
  else window.history.pushState(null, "", to);
  window.dispatchEvent(new Event(NAVIGATE_EVENT));
  window.scrollTo({ top: 0 });
}

export function Link({ to, onClick, ...rest }: AnchorHTMLAttributes<HTMLAnchorElement> & { to: string }) {
  return (
    <a
      href={to}
      onClick={(event: MouseEvent<HTMLAnchorElement>) => {
        onClick?.(event);
        const plainClick = event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey;
        if (event.defaultPrevented || !plainClick) return;
        event.preventDefault();
        navigate(to);
      }}
      {...rest}
    />
  );
}

/** Matches "/admin/people/:login" against a path; returns the decoded parameters or null. */
export function matchPath(pattern: string, path: string): Record<string, string> | null {
  const trim = (value: string) => value.replace(/\/+$/, "") || "/";
  const expected = trim(pattern).split("/");
  const actual = trim(path).split("/");
  if (expected.length !== actual.length) return null;
  const params: Record<string, string> = {};
  for (const [index, part] of expected.entries()) {
    const value = actual[index] ?? "";
    if (part.startsWith(":")) {
      if (!value) return null;
      try {
        params[part.slice(1)] = decodeURIComponent(value);
      } catch {
        return null;
      }
    } else if (part !== value) {
      return null;
    }
  }
  return params;
}
