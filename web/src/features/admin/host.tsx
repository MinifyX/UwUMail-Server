import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Cancelled, usePasswordConfirmation } from "@/features/security/ConfirmPassword";
import { useT } from "@/i18n";
import { api, type HostMachine, type HostView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

/** The helper this portal was released with; an older one is offered an update. */
export const HELPER_VERSION = 3;

export const hostKey = ["admin", "host"] as const;

/** What the portal may ask the helper for from the machine and update pages. */
export type HostVerb = "os-update" | "reboot" | "uwumail-update" | "helper-update";

/** While a job runs, ask often enough that its output moves. */
const BUSY_MS = 2000;

export function jobBusy(state: string | undefined): boolean {
  return state === "running" || state === "waiting";
}

/**
 * The machine and the job the helper works on. While a job runs this keeps asking, also through the
 * minute a UwUMail update takes the server away: the last answer stays, and the next one comes from
 * the new version.
 */
export function useHost() {
  return useQuery({
    queryKey: hostKey,
    queryFn: () => api<HostView>("/api/admin/host"),
    retry: false,
    refetchInterval: (query) => (jobBusy(query.state.data?.job?.state) ? BUSY_MS : false),
  });
}

export function helperCan(machine: HostMachine | null | undefined, verb: string): boolean {
  return Boolean(machine?.verbs?.includes(verb));
}

/** Whether the helper is older than this portal. */
export function helperOutdated(machine: HostMachine | null | undefined): boolean {
  return Boolean(machine) && Number(machine?.helper ?? "1") < HELPER_VERSION;
}

/** What to run once on the machine, where there is no helper or one too old to update itself. */
export function updateCommand(machine: HostMachine | null | undefined): string {
  return `cd ${machine?.composeDir || "/opt/uwumail"} && sudo bash update.sh`;
}

/** Asks the helper for a job, after the password: every one of them can take the server away. */
export function useAskHost(onAsked?: (verb: HostVerb) => void) {
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const { confirmed, dialog } = usePasswordConfirmation();
  const ask = useMutation({
    mutationFn: (verb: HostVerb) =>
      confirmed((password) => api<HostView>("/api/admin/host/jobs", { method: "POST", body: { verb, password } })),
    onSuccess: (view, verb) => {
      queryClient.setQueryData(hostKey, view);
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
      onAsked?.(verb);
    },
    onError: (error) => {
      if (!(error instanceof Cancelled)) toast(errorText(error), "error");
    },
  });
  return { ask, dialog };
}

/** How the last job went, with what it printed. */
export function JobBox({ view }: { view: HostView }) {
  const { t } = useT();
  const { job, log } = view;
  if (!job) return null;
  return (
    <div className="rounded-control bg-canvas px-3 py-2">
      <p className="text-[13px] font-semibold">
        {t(`host.states.${job.state}`, { defaultValue: job.state })}
        {job.error && <span className="ml-2 font-normal text-danger">{job.error}</span>}
      </p>
      {log && <pre className="mt-2 max-h-64 overflow-auto font-mono text-[12px] whitespace-pre-wrap">{log}</pre>}
    </div>
  );
}
