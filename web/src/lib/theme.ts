import { useEffect } from "react";
import { useMediaQuery } from "@/lib/media";
import { usePrefs } from "@/state/prefs";

const darkQuery = "(prefers-color-scheme: dark)";
const reducedMotionQuery = "(prefers-reduced-motion: reduce)";

/** Mirrors theme and motion preferences onto <html data-theme data-motion>, where the styles switch. */
export function useApplyTheme() {
  const theme = usePrefs((s) => s.theme);
  const motion = usePrefs((s) => s.motion);
  const systemDark = useMediaQuery(darkQuery);
  const systemReduced = useMediaQuery(reducedMotionQuery);
  const resolvedTheme = theme === "system" ? (systemDark ? "dark" : "light") : theme;
  const resolvedMotion =
    motion === "system" ? (systemReduced ? "reduced" : "full") : motion === "on" ? "full" : "reduced";
  useEffect(() => {
    document.documentElement.dataset.theme = resolvedTheme;
    document.documentElement.dataset.motion = resolvedMotion;
  }, [resolvedTheme, resolvedMotion]);
}
