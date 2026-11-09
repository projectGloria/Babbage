import type { UpdateInfo, UpdateStatus } from "../types";

/**
 * Updates are disabled in Babbage.
 */
export async function checkForUpdates(): Promise<UpdateInfo> {
  return {
    current_version: "1.0.16",
    latest_version: "1.0.16",
    has_update: false,
    release_name: "",
    release_notes: "",
    release_url: "",
    published_at: "",
    asset_name: null,
    asset_url: null,
    asset_size: null,
  };
}

export async function downloadUpdate(_assetUrl: string, _assetName: string): Promise<UpdateStatus> {
  return {
    busy: false,
    ready_to_install: false,
    downloaded_path: null,
    error: "Updates disabled",
    progress: null,
  };
}

export async function getUpdaterStatus(): Promise<UpdateStatus> {
  return {
    busy: false,
    ready_to_install: false,
    downloaded_path: null,
    error: null,
    progress: null,
  };
}

export async function installUpdateAndRestart(): Promise<void> {}

export function formatBytes(bytes?: number | null): string {
  if (!bytes || bytes <= 0) return "";
  const mb = bytes / (1024 * 1024);
  if (mb >= 1) {
    return `${mb.toFixed(1)} MB`;
  }
  const kb = bytes / 1024;
  return `${Math.round(kb)} KB`;
}

export function formatReleaseDate(isoDate?: string | null): string {
  if (!isoDate) return "";
  try {
    const d = new Date(isoDate);
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  } catch {
    return isoDate;
  }
}
