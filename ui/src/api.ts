// Thin typed wrapper around the trove-serverd HTTP/JSON API.
// All requests go through the Vite `/api` proxy to the local daemon.

export interface Metadata {
  title?: string | null;
  artist?: string | null;
  album?: string | null;
  genre?: string | null;
  year?: number | null;
  bpm?: number | null;
  key?: string | null;
  duration_secs?: number | null;
  comment?: string | null;
  file_type?: string | null;
}

export interface ArchiveEntry {
  track_id: string;
  object_key: string;
  size_bytes: number;
  sha256: string;
  metadata: Metadata;
  tags: string[];
  imported_at: string;
  updated_at: string;
}

export interface Playlist {
  id: string;
  name: string;
  created_at: string;
  updated_at: string;
  track_ids: string[];
}

export interface QuerySpec {
  text?: string;
  artist?: string;
  genre?: string;
  key?: string;
  bpm?: { min?: number; max?: number };
  limit?: number;
}

const BASE = "/api";

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    headers: { "content-type": "application/json" },
    ...init,
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    throw new Error((data as { error?: string }).error ?? res.statusText);
  }
  return data as T;
}

export const api = {
  health: () => request<{ status: string }>("/health"),

  reconcile: (offline = false) =>
    request<{ report: string }>("/reconcile", {
      method: "POST",
      body: JSON.stringify({ offline }),
    }),

  query: (spec: QuerySpec, offline = false) =>
    request<{ tracks: ArchiveEntry[] }>("/query", {
      method: "POST",
      body: JSON.stringify({ ...spec, offline }),
    }).then((r) => r.tracks),

  listPlaylists: () =>
    request<{ playlists: Playlist[] }>("/playlists").then((r) => r.playlists),

  createPlaylist: (name: string) =>
    request<{ playlist: Playlist }>("/playlists", {
      method: "POST",
      body: JSON.stringify({ name }),
    }).then((r) => r.playlist),

  addTracks: (name: string, trackIds: string[]) =>
    request<{ added: number }>(`/playlists/${encodeURIComponent(name)}/tracks`, {
      method: "POST",
      body: JSON.stringify({ track_ids: trackIds }),
    }),

  import: (path: string, planOnly = false) =>
    request<{ job_id: string; committed?: number; total?: number }>("/import", {
      method: "POST",
      body: JSON.stringify({ path, plan_only: planOnly }),
    }),
};
