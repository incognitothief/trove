import { useCallback, useEffect, useMemo, useState } from "react";
import { api, type ArchiveEntry, type Playlist, type QuerySpec } from "./api";

export function App() {
  const [status, setStatus] = useState<string>("connecting…");
  const [tracks, setTracks] = useState<ArchiveEntry[]>([]);
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [text, setText] = useState("");
  const [artist, setArtist] = useState("");
  const [genre, setGenre] = useState("");
  const [bpmMin, setBpmMin] = useState("");
  const [bpmMax, setBpmMax] = useState("");

  const refreshPlaylists = useCallback(async () => {
    try {
      setPlaylists(await api.listPlaylists());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    api
      .health()
      .then((h) => setStatus(h.status))
      .catch(() => setStatus("offline (is trove-serverd running?)"));
    refreshPlaylists();
  }, [refreshPlaylists]);

  const runSearch = useCallback(async () => {
    setBusy(true);
    setError(null);
    const spec: QuerySpec = { limit: 200 };
    if (text) spec.text = text;
    if (artist) spec.artist = artist;
    if (genre) spec.genre = genre;
    const min = bpmMin ? Number(bpmMin) : undefined;
    const max = bpmMax ? Number(bpmMax) : undefined;
    if (min !== undefined || max !== undefined) spec.bpm = { min, max };
    try {
      setTracks(await api.query(spec));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [text, artist, genre, bpmMin, bpmMax]);

  const toggle = (id: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      next.has(id) ? next.delete(id) : next.add(id);
      return next;
    });

  const selectedCount = selected.size;

  const createPlaylist = async () => {
    const name = prompt("New playlist name");
    if (!name) return;
    try {
      await api.createPlaylist(name);
      await refreshPlaylists();
    } catch (e) {
      setError(String(e));
    }
  };

  const addToPlaylist = async (name: string) => {
    if (selectedCount === 0) return;
    try {
      await api.addTracks(name, [...selected]);
      setSelected(new Set());
      await refreshPlaylists();
    } catch (e) {
      setError(String(e));
    }
  };

  const importFolder = async () => {
    const path = prompt("Absolute path of folder to import");
    if (!path) return;
    setBusy(true);
    setError(null);
    try {
      const res = await api.import(path);
      setStatus(`imported job ${res.job_id} (committed ${res.committed ?? 0})`);
      await runSearch();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const online = useMemo(() => status === "ok", [status]);

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="logo">◆</span> Trove
        </div>
        <div className={`status ${online ? "ok" : "bad"}`}>
          daemon: {status}
        </div>
        <div className="actions">
          <button onClick={importFolder} disabled={busy}>
            Import folder…
          </button>
        </div>
      </header>

      {error && <div className="error">{error}</div>}

      <div className="layout">
        <aside className="sidebar">
          <div className="sidebar-head">
            <h2>Playlists</h2>
            <button className="ghost" onClick={createPlaylist}>
              +
            </button>
          </div>
          {playlists.length === 0 && <p className="muted">No playlists yet.</p>}
          <ul className="playlist-list">
            {playlists.map((pl) => (
              <li key={pl.id}>
                <button
                  className="playlist"
                  disabled={selectedCount === 0}
                  title={
                    selectedCount === 0
                      ? "Select tracks to add"
                      : `Add ${selectedCount} track(s)`
                  }
                  onClick={() => addToPlaylist(pl.name)}
                >
                  <span>{pl.name}</span>
                  <span className="count">{pl.track_ids.length}</span>
                </button>
              </li>
            ))}
          </ul>
        </aside>

        <main className="content">
          <form
            className="search"
            onSubmit={(e) => {
              e.preventDefault();
              runSearch();
            }}
          >
            <input
              placeholder="Search text…"
              value={text}
              onChange={(e) => setText(e.target.value)}
            />
            <input
              placeholder="Artist"
              value={artist}
              onChange={(e) => setArtist(e.target.value)}
            />
            <input
              placeholder="Genre"
              value={genre}
              onChange={(e) => setGenre(e.target.value)}
            />
            <input
              className="bpm"
              placeholder="BPM min"
              value={bpmMin}
              onChange={(e) => setBpmMin(e.target.value)}
            />
            <input
              className="bpm"
              placeholder="BPM max"
              value={bpmMax}
              onChange={(e) => setBpmMax(e.target.value)}
            />
            <button type="submit" disabled={busy}>
              {busy ? "…" : "Search"}
            </button>
          </form>

          <div className="results-head">
            <span>
              {tracks.length} track{tracks.length === 1 ? "" : "s"}
            </span>
            {selectedCount > 0 && (
              <span className="selected">{selectedCount} selected</span>
            )}
          </div>

          <table className="tracks">
            <thead>
              <tr>
                <th />
                <th>Artist</th>
                <th>Title</th>
                <th>Album</th>
                <th>Genre</th>
                <th>BPM</th>
                <th>Key</th>
              </tr>
            </thead>
            <tbody>
              {tracks.map((t) => (
                <tr
                  key={t.track_id}
                  className={selected.has(t.track_id) ? "row sel" : "row"}
                  onClick={() => toggle(t.track_id)}
                >
                  <td>
                    <input
                      type="checkbox"
                      readOnly
                      checked={selected.has(t.track_id)}
                    />
                  </td>
                  <td>{t.metadata.artist ?? "—"}</td>
                  <td>{t.metadata.title ?? "—"}</td>
                  <td>{t.metadata.album ?? "—"}</td>
                  <td>{t.metadata.genre ?? "—"}</td>
                  <td>{t.metadata.bpm ?? "—"}</td>
                  <td>{t.metadata.key ?? "—"}</td>
                </tr>
              ))}
              {tracks.length === 0 && (
                <tr>
                  <td colSpan={7} className="muted center">
                    No results. Try a search, or import a folder to seed the
                    archive.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </main>
      </div>
    </div>
  );
}
