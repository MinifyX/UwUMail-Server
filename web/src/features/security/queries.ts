import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type SecurityView } from "@/lib/api";

export const securityKey = ["account", "security"] as const;

export function useSecurity() {
  return useQuery({ queryKey: securityKey, queryFn: () => api<SecurityView>("/api/account/security") });
}

/** A change on the security page; the page reloads its data afterwards. */
export function useSecurityAction<Input, Output>(run: (input: Input) => Promise<Output>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: run,
    onSettled: () => void queryClient.invalidateQueries({ queryKey: securityKey }),
  });
}
