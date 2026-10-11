import { describe, expect, it } from "vitest";
import { afterInstall } from "$lib/install";
import type { Report } from "$lib/ipc";

function report(state: Report["state"], bundleId: string | null): Report {
  return {
    state,
    profile_id: null,
    public_ip: null,
    killswitch_engaged: state !== "disconnected",
    bundle: bundleId ? { bundle_id: bundleId, bundle_name: "In use", profiles: [] } : null,
    auto_rotate: true,
    interval_minutes: 60,
    rotatable: true,
    runtimes: [],
  };
}

describe("afterInstall", () => {
  it("leaves the bundle in use alone", () => {
    expect(afterInstall(report("connected", "new"), "new")).toBe("in_use");
    expect(afterInstall(report("disconnected", "new"), "new")).toBe("in_use");
  });

  it("switches on its own when nothing is connected", () => {
    expect(afterInstall(report("disconnected", "old"), "new")).toBe("switch");
    expect(afterInstall(report("disconnected", null), "new")).toBe("switch");
  });

  it("offers the switch when it would drop a tunnel or lift a block", () => {
    expect(afterInstall(report("connected", "old"), "new")).toBe("offer");
    expect(afterInstall(report("blocked", "old"), "new")).toBe("offer");
    // A block left by a crash, with no bundle in use: switching would lift it.
    expect(afterInstall(report("blocked", null), "new")).toBe("offer");
  });

  it("offers it when the service cannot say what is going on", () => {
    expect(afterInstall(null, "new")).toBe("offer");
  });
});
