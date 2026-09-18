import { useSyncExternalStore } from "react";

/** Follows a CSS media query from a component, for the cases where hiding with a class is not enough. */
export function useMediaQuery(query: string) {
  return useSyncExternalStore(
    (callback) => {
      const media = window.matchMedia(query);
      media.addEventListener("change", callback);
      return () => media.removeEventListener("change", callback);
    },
    () => window.matchMedia(query).matches,
  );
}

/** Narrower than Tailwind's `sm`: a phone held upright, where a wide table has nowhere to go. */
export function usePhone() {
  return useMediaQuery("(max-width: 639px)");
}
