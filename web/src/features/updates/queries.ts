import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Cancelled, usePasswordConfirmation } from "@/features/security/ConfirmPassword";
import { api, type UpdateStepKind, type UpdatesView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

export const updatesKey = ["admin", "updates"] as const;

/** Whether a run is still going, or waits for an answer. */
export function runActive(view: UpdatesView | undefined): boolean {
  return Boolean(view?.run && view.run.finishedAt === null);
}

/**
 * What runs here, what is newer, and the run that installs it. While a run goes this keeps asking,
 * also through the minute the server's own update takes it away: the last answer stays on screen.
 */
export function useUpdates() {
  return useQuery({
    queryKey: updatesKey,
    queryFn: () => api<UpdatesView>("/api/admin/updates"),
    retry: false,
    refetchInterval: (query) => (runActive(query.state.data) ? 2000 : false),
  });
}

export type StepAnswer = { action: "retry" | "skip" | "stop"; compose?: "keep" | "replace" };

/** Starts a run and answers a step that failed; both after the password, as either can take the server away. */
export function useUpdateRun() {
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const { confirmed, dialog } = usePasswordConfirmation();
  const options = {
    onSuccess: (view: UpdatesView) => {
      queryClient.setQueryData(updatesKey, view);
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
    },
    onError: (error: unknown) => {
      if (!(error instanceof Cancelled)) toast(errorText(error), "error");
    },
  };
  /** Without steps: everything that waits. */
  const start = useMutation({
    mutationFn: (steps?: UpdateStepKind[]) =>
      confirmed((password) =>
        api<UpdatesView>("/api/admin/updates/run", { method: "POST", body: { steps, password } }),
      ),
    ...options,
  });
  const answer = useMutation({
    mutationFn: (body: StepAnswer) =>
      confirmed((password) =>
        api<UpdatesView>("/api/admin/updates/run/step", { method: "POST", body: { ...body, password } }),
      ),
    ...options,
  });
  return { start, answer, dialog };
}
