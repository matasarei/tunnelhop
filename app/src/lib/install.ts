// What the window does with a bundle that was just installed: an import, or a new bundle made
// of config files.
import type { Report } from "$lib/ipc";

/** Already the bundle in use; switch to it now; or offer the switch and leave it to the user. */
export type AfterInstall = "in_use" | "switch" | "offer";

/**
 * Switching disconnects the current bundle and lifts its block first. With nothing connected
 * that costs nothing, so it happens on its own; with a tunnel up or traffic blocked, or with
 * the service out of reach, it is the user's call.
 */
export function afterInstall(report: Report | null, bundleId: string): AfterInstall {
  if (report?.bundle?.bundle_id === bundleId) return "in_use";
  return report?.state === "disconnected" ? "switch" : "offer";
}
