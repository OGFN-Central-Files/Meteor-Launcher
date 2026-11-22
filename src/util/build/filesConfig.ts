import { Config } from "../config";
import { api } from "../../lib/api";
import { useConfigStore } from "../../stores/settings";

export type PakEntry = { name: string; url: string; size: number };

// Launcher-managed files that do NOT come from the server pak manifest: the
// redirect DLL and any extra injected DLLs. Shared by launch.ts and verify.ts so
// both agree on what the launcher owns.
export function getManagedFilesConfig() {
  return {
    redirectLink: Config.LAUNCH_OPTIONS.REDIRECT_DOWNLOAD || "",
    useCustomPaks: false,
    customPaksLinks: [] as string[],
    injectExtraDlls: Config.LAUNCH_OPTIONS.DOWNLOAD_EXTRA_DLLS || false,
    extraDllLinks: Config.LAUNCH_OPTIONS.DLL_LINKS || [],
  };
}

function filterPaks(paks: unknown): PakEntry[] {
  if (!Array.isArray(paks)) return [];
  return paks.filter(
    (p): p is PakEntry =>
      !!p && typeof p.name === "string" && typeof p.url === "string",
  );
}

// Required game paks, fetched from the backend so they can be updated monthly
// without a new launcher build. Missing/wrong-size files are (re)downloaded for
// everyone; the download itself can't be canceled (game files).
export async function fetchPaksManifest(): Promise<PakEntry[]> {
  const res = await api.getPaks();
  return res.success && res.data ? filterPaks(res.data.paks) : [];
}

// Optional "Mobile Builds" paks — same server-hosted mechanism, but only pulled
// when the player enables the toggle in Settings.
export async function fetchMobilePaksManifest(): Promise<PakEntry[]> {
  const res = await api.getMobilePaks();
  return res.success && res.data ? filterPaks(res.data.paks) : [];
}

// Optional "Bubble Builds" paks — same mechanism as Mobile Builds, its own toggle.
export async function fetchBubblePaksManifest(): Promise<PakEntry[]> {
  const res = await api.getBubblePaks();
  return res.success && res.data ? filterPaks(res.data.paks) : [];
}

// The full set of paks to (re)download this launch: always the required paks, plus
// each optional channel (Mobile / Bubble Builds) whose toggle is on. Merged by name
// (required wins) so both launch and verify agree on the same list.
export async function fetchPaksToApply(): Promise<PakEntry[]> {
  const { mobileBuilds, bubbleBuilds } = useConfigStore.getState();
  const byName = new Map<string, PakEntry>();
  for (const p of await fetchPaksManifest()) byName.set(p.name, p);
  if (mobileBuilds) for (const p of await fetchMobilePaksManifest()) if (!byName.has(p.name)) byName.set(p.name, p);
  if (bubbleBuilds) for (const p of await fetchBubblePaksManifest()) if (!byName.has(p.name)) byName.set(p.name, p);
  return [...byName.values()];
}

// Filenames of OPTIONAL paks whose channel is currently OFF, so the launcher can DELETE
// them from the game's Paks folder at launch — turning a toggle off actually removes its
// content. Never returns a required pak (safety: required always wins).
export async function getOptionalPaksToRemove(): Promise<string[]> {
  const { mobileBuilds, bubbleBuilds } = useConfigStore.getState();
  const required = new Set((await fetchPaksManifest()).map((p) => p.name));
  const remove = new Set<string>();
  if (!mobileBuilds) for (const p of await fetchMobilePaksManifest()) if (!required.has(p.name)) remove.add(p.name);
  if (!bubbleBuilds) for (const p of await fetchBubblePaksManifest()) if (!required.has(p.name)) remove.add(p.name);
  return [...remove];
}
