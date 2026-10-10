// The app's commands, typed. Everything the window does goes through here to the Rust side,
// which talks to the service; the shapes mirror th-core's `ipc` types.
import { invoke } from "@tauri-apps/api/core";

export type Protocol = "wireguard" | "openvpn";

export type Profile = {
  id: string;
  name: string;
  protocol: Protocol;
  /** False for an OpenVPN profile still waiting for a username and password. */
  has_credentials: boolean;
};

export type Bundle = { bundle_id: string; bundle_name: string; profiles: Profile[] };

export type Availability =
  | { status: "available"; version: string }
  | { status: "missing" }
  | { status: "too_old"; found: string; required: string };

export type Report = {
  state: "connected" | "blocked" | "disconnected";
  profile_id: string | null;
  public_ip: string | null;
  killswitch_engaged: boolean;
  bundle: Bundle | null;
  auto_rotate: boolean;
  interval_minutes: number;
  runtimes: { protocol: Protocol; availability: Availability }[];
};

export type Action =
  | { type: "connect"; profile_id?: string }
  | { type: "rotate" }
  | { type: "disconnect" }
  | { type: "set_auto_rotate"; enabled: boolean }
  | { type: "set_interval"; minutes: number }
  | { type: "exit" };

export type ImportOutcome =
  | { type: "imported"; bundle: Bundle; already_present: boolean; needs_credentials: string[] }
  | { type: "refused"; errors: string[] }
  | { type: "cancelled" };

export const protocolName: Record<Protocol, string> = {
  wireguard: "WireGuard",
  openvpn: "OpenVPN",
};

export const status = () => invoke<Report | null>("status");
export const intervals = () => invoke<[number, string][]>("intervals");
export const act = (action: Action) => invoke<void>("act", { action });

/** Import the file at `path` (dropped on the window), or one chosen in the native picker. */
export const importBundle = (path?: string) =>
  invoke<ImportOutcome>("import_bundle", { path: path ?? null });

export const selectBundle = (bundleId: string) => invoke<void>("select_bundle", { bundleId });

/** Every installed bundle, ordered by name, and the id of the active one. */
export type BundleList = { bundles: Bundle[]; active: string | null };

export const listBundles = () => invoke<BundleList>("list_bundles");

/** Delete a bundle and its saved sign-in; the list that is left comes back. */
export const deleteBundle = (bundleId: string) => invoke<BundleList>("delete_bundle", { bundleId });

/** A config file to add as a server, and the name the server will show under. */
export type ChosenFile = { path: string; name: string };

/** The dropped config files, or ones chosen in the native picker; empty when it was closed. */
export const chooseServers = (paths?: string[]) =>
  invoke<ChosenFile[]>("choose_servers", { paths: paths ?? null });

/** Add servers to a bundle. It is replaced by a new bundle, as an import would make it. */
export const addServers = (bundleId: string, files: ChosenFile[]) =>
  invoke<ImportOutcome>("add_servers", { bundleId, files });

export const removeServers = (bundleId: string, profileIds: string[]) =>
  invoke<ImportOutcome>("remove_servers", { bundleId, profileIds });

/** For the listed profiles, or as the bundle's default for every one when the list is empty. */
export const setCredentials = (
  bundleId: string,
  profileIds: string[],
  username: string,
  password: string,
) => invoke<void>("set_credentials", { bundleId, profileIds, username, password });

/** Why a profile's protocol cannot be used on this PC, or null when it can. */
export function runtimeProblem(report: Report, protocol: Protocol): string | null {
  const runtime = report.runtimes.find((r) => r.protocol === protocol)?.availability;
  const name = protocolName[protocol];
  if (!runtime || runtime.status === "missing") return `${name} is not installed`;
  if (runtime.status === "too_old")
    return `${name} ${runtime.found} is too old (${runtime.required} or newer is needed)`;
  return null;
}
