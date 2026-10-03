import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useT } from "@/i18n";
import { api, type MoveDetail, type MovesView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";
import { isBusy } from "./moves";

export const movesKey = ["admin", "moves"] as const;
const moveKey = (id: number) => ["admin", "moves", id] as const;

export function useMoves() {
  return useQuery({
    queryKey: movesKey,
    queryFn: () => api<MovesView>("/api/admin/moves"),
    refetchInterval: (query) => (query.state.data?.moves.some(isBusy) ? 5000 : 30000),
  });
}

export function useMove(id: number) {
  return useQuery({
    queryKey: moveKey(id),
    queryFn: () => api<MoveDetail>(`/api/admin/moves/${id}`),
    // While something is copied the page follows it closely; otherwise now and then.
    refetchInterval: (query) => (query.state.data && isBusy(query.state.data.move) ? 5000 : 30000),
  });
}

/** A change to a move that answers with the move, with a toast when it went wrong (and one when it worked). */
export function useMoveAction<Input>(id: number, run: (input: Input) => Promise<MoveDetail>, success?: string) {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: run,
    onSuccess: (detail) => {
      queryClient.setQueryData(moveKey(id), detail);
      void queryClient.invalidateQueries({ queryKey: movesKey, exact: true });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
      if (success) toast(success, "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

/** Removing a move; the list is shown afterwards. */
export function useDeleteMove(id: number, onDone: () => void) {
  const { t } = useT();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: () => api<void>(`/api/admin/moves/${id}`, { method: "DELETE" }),
    onSuccess: () => {
      queryClient.removeQueries({ queryKey: moveKey(id) });
      void queryClient.invalidateQueries({ queryKey: movesKey, exact: true });
      toast(t("moves.toasts.deleted"), "success");
      onDone();
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}
