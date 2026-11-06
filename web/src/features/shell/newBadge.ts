import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type Session } from "@/lib/api";

/** The numbers of a version like `0.26.0`; a pre-release suffix counts as that release. */
function parts(version: string): number[] {
  return version
    .replace(/-.*/, "")
    .split(".")
    .map((part) => Number.parseInt(part, 10) || 0);
}

/** Whether version `a` is older than `b`. */
export function olderThan(a: string, b: string): boolean {
  const [x, y] = [parts(a), parts(b)];
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const diff = (x[i] ?? 0) - (y[i] ?? 0);
    if (diff !== 0) return diff < 0;
  }
  return false;
}

/** The "New" tags this person has already clicked away, on any device. */
export function seenFeaturesOf(preferences: Record<string, unknown> | undefined): string[] {
  const value = preferences?.seenFeatures;
  return Array.isArray(value) ? value.filter((id): id is string => typeof id === "string") : [];
}

/**
 * Whether a menu entry gets the "New" tag: only when it came with an update of this server (it
 * was installed with an older version) and the person has not opened it yet.
 */
export function showsNew(
  feature: { id: string; since: string },
  installedVersion: string | undefined,
  seen: string[],
): boolean {
  return olderThan(installedVersion ?? "0.0.0", feature.since) && !seen.includes(feature.id);
}

/** Remembers that a "New" tag was clicked; the tag goes at once, a failure brings it back. */
export function useMarkSeen() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (seenFeatures: string[]) =>
      api<Record<string, unknown>>("/api/account/preferences", { method: "PATCH", body: { seenFeatures } }),
    onMutate: (seenFeatures) => {
      const previous = queryClient.getQueryData<Session | null>(["session"]);
      if (previous) {
        queryClient.setQueryData<Session>(["session"], {
          ...previous,
          preferences: { ...previous.preferences, seenFeatures },
        });
      }
      return previous;
    },
    onSuccess: (preferences) => {
      const current = queryClient.getQueryData<Session | null>(["session"]);
      if (current) queryClient.setQueryData<Session>(["session"], { ...current, preferences });
    },
    onError: (_error, _seen, previous) => {
      if (previous) queryClient.setQueryData(["session"], previous);
    },
  });
}
