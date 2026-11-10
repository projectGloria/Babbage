import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

/* Thumbnail resolution shared by result rows, grid cards and the preview
   pane. The backend (`get_thumbnail`) returns an absolute cached path which
   is converted once per path into an asset URL. Everything degrades
   silently to the caller rendering the plain file icon. */

/** Extensions the backend thumbnailer handles. Everything else never asks. */
const THUMB_EXTS = new Set([
  "png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif",
  "pdf",
]);

export const THUMBNAIL_LAZY_LIMIT = 30;

export function isThumbImage(filename: string): boolean {
  if (!filename.includes(".")) return false;
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  return THUMB_EXTS.has(ext);
}



export interface ThumbnailState {
  /** Resolved asset URL, or null until the backend answers. */
  src: string | null;
  /** True once after a clean failure (missing command, backend error) —
      callers then fall back to the plain icon. Never logs. */
  failed: boolean;
}

/* Session cache shared by every card and the preview pane. The grid is
   virtualised, so scrolling away and back remounts cards; without this each
   remount repeated the backend freshness check, file read, base64 encode and
   IPC. Bounded LRU (Map keeps insertion order); in-flight requests are
   shared so a card and the preview never fetch the same path twice. */
const CACHE_LIMIT = 400;
const cache = new Map<string, string>();
const inflight = new Map<string, Promise<string | null>>();

function cached(path: string): string | null {
  const hit = cache.get(path);
  if (hit === undefined) return null;
  cache.delete(path);
  cache.set(path, hit);
  return hit;
}

function remember(path: string, dataUrl: string) {
  cache.set(path, dataUrl);
  if (cache.size > CACHE_LIMIT) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
}

function fetchThumbnail(path: string): Promise<string | null> {
  const pending = inflight.get(path);
  if (pending) return pending;
  const request = invoke<string | null>("get_thumbnail_data", { path })
    .then((dataUrl) => {
      if (dataUrl) remember(path, dataUrl);
      return dataUrl;
    })
    .finally(() => inflight.delete(path));
  inflight.set(path, request);
  return request;
}

/** Test hook: forget every cached thumbnail. */
export function clearThumbnailCache() {
  cache.clear();
  inflight.clear();
}

/**
 * Resolves the cached thumbnail for one file path: from the session cache
 * when present, otherwise a single shared `get_thumbnail_data` request.
 * `enabled=false` (non-image files, rows below the lazy limit) short-
 * circuits before any call is made.
 */
export function useThumbnail(path: string, enabled: boolean): ThumbnailState {
  const thumbable = enabled && isThumbImage(path);
  const [src, setSrc] = useState<string | null>(() => (thumbable ? cached(path) : null));
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!thumbable) return;
    const hit = cached(path);
    if (hit) {
      setSrc(hit);
      return;
    }
    let cancelled = false;
    fetchThumbnail(path)
      .then((dataUrl) => {
        if (cancelled) return;
        if (dataUrl) {
          setSrc(dataUrl);
        } else {
          setFailed(true);
        }
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [path, thumbable]);

  return { src, failed };
}
