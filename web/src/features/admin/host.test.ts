import { describe, expect, it } from "vitest";
import type { HostMachine } from "@/lib/api";
import { HELPER_VERSION, helperCan, helperOutdated, jobBusy, updateCommand } from "./host";

const machine = (changes: Partial<HostMachine>): HostMachine => ({
  kind: "debian",
  name: "Debian",
  updates: 0,
  securityUpdates: 0,
  rebootRequired: false,
  rebootPackages: [],
  alone: true,
  others: [],
  image: "",
  digest: "",
  composeDir: "/srv/mail",
  checkedAt: 0,
  ...changes,
});

describe("the machine's helper", () => {
  it("is outdated when older than this portal, and a helper without a version is the first one", () => {
    expect(helperOutdated(machine({ helper: "2" }))).toBe(true);
    expect(helperOutdated(machine({}))).toBe(true);
    expect(helperOutdated(machine({ helper: String(HELPER_VERSION) }))).toBe(false);
    expect(helperOutdated(null)).toBe(false);
  });

  it("does only the verbs it lists", () => {
    const old = machine({ verbs: ["os-update", "reboot"] });
    expect(helperCan(old, "reboot")).toBe(true);
    expect(helperCan(old, "uwumail-update")).toBe(false);
    expect(helperCan(undefined, "reboot")).toBe(false);
  });

  it("names the directory UwUMail runs from in the command", () => {
    expect(updateCommand(machine({}))).toBe("cd /srv/mail && sudo bash update.sh");
    expect(updateCommand(null)).toBe("cd /opt/uwumail && sudo bash update.sh");
  });

  it("counts a job as busy until the helper answered", () => {
    expect(["waiting", "running", "done", "failed", undefined].map(jobBusy)).toEqual([true, true, false, false, false]);
  });
});
