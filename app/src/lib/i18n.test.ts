import { beforeEach, describe, expect, it } from "vitest";
import { sentences, setLanguage, t, tn } from "$lib/i18n";
import { runtimeProblem, type Report } from "$lib/ipc";

// `setLanguage` marks the page's language; the tests run without a page.
beforeEach(() => {
  (globalThis as { document?: unknown }).document = { documentElement: {} };
  setLanguage("en");
});

describe("t", () => {
  it("fills placeholders, once", () => {
    expect(t("notice.deleted", { bundle: "Lab" })).toBe('Deleted "Lab".');
    // A value that itself reads like a placeholder stays as it is.
    expect(t("notice.deleted", { bundle: "{bundle}" })).toBe('Deleted "{bundle}".');
  });

  it("leaves a placeholder it has no value for", () => {
    expect(t("notice.deleted")).toBe('Deleted "{bundle}".');
  });

  it("speaks the language it is given, and English for one it does not have", () => {
    setLanguage("uk");
    expect(t("action.delete")).toBe("Видалити");
    setLanguage("tlh");
    expect(t("action.delete")).toBe("Delete");
  });
});

describe("tn", () => {
  it("picks the English forms", () => {
    expect(tn("bundles.servers", 1)).toBe("1 server");
    expect(tn("bundles.servers", 3)).toBe("3 servers");
  });

  it("picks Ukrainian's three forms by the number's ending", () => {
    setLanguage("uk");
    expect(tn("bundles.servers", 1)).toMatch(/^1 /);
    expect(tn("bundles.servers", 21)).toBe(tn("bundles.servers", 1).replace("1", "21"));
    expect(tn("bundles.servers", 3)).not.toBe(tn("bundles.servers", 5).replace("5", "3"));
    expect(tn("bundles.servers", 11)).toBe(tn("bundles.servers", 5).replace("5", "11"));
  });

  it("falls back to the other form where a language has only that", () => {
    setLanguage("ja");
    expect(tn("bundles.servers", 1)).toContain("1");
    expect(tn("bundles.servers", 1)).toBe(tn("bundles.servers", 2).replace("2", "1"));
  });
});

describe("sentences", () => {
  it("drops the parts that do not apply", () => {
    expect(sentences("One.", false, null, undefined, "", "Two.")).toBe("One. Two.");
  });

  it("puts nothing between sentences in Japanese and Chinese", () => {
    setLanguage("ja");
    expect(sentences("一。", "二。")).toBe("一。二。");
    setLanguage("zh-Hans");
    expect(sentences("一。", "二。")).toBe("一。二。");
  });
});

describe("runtimeProblem", () => {
  const withRuntime = (availability: Report["runtimes"][number]["availability"] | null): Report => ({
    state: "disconnected",
    profile_id: null,
    public_ip: null,
    killswitch_engaged: false,
    bundle: null,
    auto_rotate: true,
    interval_minutes: 60,
    rotatable: false,
    runtimes: availability ? [{ protocol: "openvpn", availability }] : [],
  });

  it("says nothing when the program is there", () => {
    expect(runtimeProblem(withRuntime({ status: "available", version: "2.6.14" }), "openvpn")).toBeNull();
  });

  it("names the program the protocol needs", () => {
    expect(runtimeProblem(withRuntime({ status: "missing" }), "openvpn")).toBe(
      "OpenVPN Community is not installed",
    );
    expect(runtimeProblem(withRuntime(null), "wireguard")).toBe("WireGuard is not installed");
  });

  it("tells a version too old from one that could not be read", () => {
    const old = withRuntime({ status: "too_old", found: "2.5.9", required: "2.6.0" });
    expect(runtimeProblem(old, "openvpn")).toBe("OpenVPN Community 2.5.9 is too old (2.6.0 or newer is needed)");
    const unread = withRuntime({ status: "too_old", found: "unknown", required: "2.6.0" });
    expect(runtimeProblem(unread, "openvpn")).toBe(
      "OpenVPN Community could not be checked (2.6.0 or newer is needed)",
    );
  });
});
