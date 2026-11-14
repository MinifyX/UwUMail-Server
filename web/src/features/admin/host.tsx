import { useQuery } from "@tanstack/react-query";
import { api, type HostMachine, type HostView } from "@/lib/api";

/** The helper this portal was released with; an older one is offered an update. */
export const HELPER_VERSION = 4;

export const hostKey = ["admin", "host"] as const;

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
  // machine.json is written where the container can write too: only a plain path goes into a
  // command somebody pastes into a root shell.
  const dir = machine?.composeDir ?? "";
  return `cd ${/^\/[A-Za-z0-9._/-]+$/.test(dir) ? dir : "/opt/uwumail"} && sudo bash update.sh`;
}
