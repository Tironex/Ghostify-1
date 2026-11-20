import { useEffect, useRef, useState } from "react";
import { call, errorText } from "../api/backend";
import type { LyricsCurrent, PlayerAction, PlayerSnapshot } from "../api/types";
import { useToast } from "../ui/toast";

const POLL_MS = 1000;

function clock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

/** Index of the last line that has started. */
export function activeLine(times: number[], position: number): number {
  let low = 0;
  let high = times.length;
  while (low < high) {
    const mid = (low + high) >> 1;
    if ((times[mid] ?? 0) <= position) low = mid + 1;
    else high = mid;
  }
  return low - 1;
}

export function MiniPlayer() {
  const toast = useToast();
  const [snapshot, setSnapshot] = useState<PlayerSnapshot | null>(null);
  const [offline, setOffline] = useState<string | null>(null);
  const [lyrics, setLyrics] = useState<LyricsCurrent | null>(null);
  const trackKey = snapshot?.track ? `${snapshot.track.artist}\u0000${snapshot.track.title}\u0000${snapshot.track.album}` : "";
  const activeRef = useRef<HTMLLIElement | null>(null);

  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const next = await call<PlayerSnapshot>("player.state");
        if (!alive) return;
        setSnapshot(next);
        setOffline(null);
      } catch (error) {
        if (alive) setOffline(errorText(error));
      }
    };
    void tick();
    const timer = window.setInterval(tick, POLL_MS);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    if (!trackKey) {
      setLyrics(null);
      return;
    }
    let alive = true;
    setLyrics(null);
    call<LyricsCurrent>("lyrics.current")
      .then((value) => alive && setLyrics(value))
      .catch(() => alive && setLyrics({ track: null, lyrics: null }));
    return () => {
      alive = false;
    };
  }, [trackKey]);

  const synced = lyrics?.lyrics?.synced ?? [];
  const current = snapshot ? activeLine(synced.map((l) => l.time_ms), snapshot.position_ms) : -1;
  useEffect(() => {
    activeRef.current?.scrollIntoView?.({ block: "center", behavior: "smooth" });
  }, [current, trackKey]);

  const send = async (action: PlayerAction) => {
    try {
      await call("player.command", { action });
      const next = await call<PlayerSnapshot>("player.state");
      setSnapshot(next);
    } catch (error) {
      toast.error(error);
    }
  };

  const track = snapshot?.track ?? null;
  const duration = track?.duration_ms ?? 0;
  const percent = duration > 0 && snapshot ? Math.min(100, (snapshot.position_ms / duration) * 100) : 0;

  return (
    <div className="flex h-screen flex-col bg-zinc-50 p-4 dark:bg-zinc-950">
      {offline && <p role="alert" className="mb-2 rounded-md bg-red-100 p-2 text-xs text-red-900 dark:bg-red-950 dark:text-red-200">{offline}</p>}

      {track ? (
        <div className="flex items-center gap-3">
          {track.artwork ? (
            <img src={track.artwork} alt="" className="h-16 w-16 rounded-md object-cover" referrerPolicy="no-referrer" />
          ) : (
            <div className="h-16 w-16 rounded-md bg-zinc-200 dark:bg-zinc-800" aria-hidden />
          )}
          <div className="min-w-0">
            <p className="truncate font-semibold" title={track.title}>{track.title}</p>
            <p className="truncate text-sm text-zinc-500">{track.artist}</p>
            <p className="truncate text-xs text-zinc-500">{track.album}</p>
          </div>
        </div>
      ) : (
        <p className="py-6 text-center text-sm text-zinc-500">Nothing is playing. Start a song in Spotify.</p>
      )}

      <div className="mt-3" aria-label="Progress">
        <div className="h-1.5 overflow-hidden rounded-full bg-zinc-200 dark:bg-zinc-800">
          <div className="h-full bg-accent" style={{ width: `${percent}%` }} role="progressbar" aria-valuenow={Math.round(percent)} aria-valuemin={0} aria-valuemax={100} />
        </div>
        <div className="mt-1 flex justify-between text-xs text-zinc-500">
          <span>{clock(snapshot?.position_ms ?? 0)}</span>
          <span>{duration > 0 ? clock(duration) : ""}</span>
        </div>
      </div>

      <div className="mt-2 flex items-center justify-center gap-3">
        <button type="button" aria-label="Previous track" onClick={() => send("previous")} className="rounded-full p-2 text-xl hover:bg-zinc-200 dark:hover:bg-zinc-800">⏮</button>
        <button type="button" aria-label={snapshot?.playing ? "Pause" : "Play"} onClick={() => send("toggle")} className="rounded-full bg-white px-4 py-2 text-xl text-black hover:bg-zinc-200">
          {snapshot?.playing ? "⏸" : "▶"}
        </button>
        <button type="button" aria-label="Next track" onClick={() => send("next")} className="rounded-full p-2 text-xl hover:bg-zinc-200 dark:hover:bg-zinc-800">⏭</button>
      </div>

      <section aria-label="Lyrics" className="mt-3 min-h-0 flex-1 overflow-y-auto rounded-lg border border-zinc-200 p-3 text-center dark:border-zinc-800">
        {!track ? null : lyrics === null ? (
          <p className="text-sm text-zinc-500">Looking for lyrics…</p>
        ) : synced.length > 0 ? (
          <ul className="space-y-2">
            {synced.map((line, index) => (
              <li
                key={index}
                ref={index === current ? activeRef : undefined}
                aria-current={index === current ? "true" : undefined}
                className={index === current ? "text-lg font-semibold" : "text-sm text-zinc-500"}
              >
                {line.text || "♪"}
              </li>
            ))}
          </ul>
        ) : lyrics.lyrics?.plain ? (
          <p className="text-sm whitespace-pre-line">{lyrics.lyrics.plain}</p>
        ) : (
          <p className="text-sm text-zinc-500">No lyrics found. Put an .lrc file named “{track.artist} - {track.title}.lrc” in your lyrics folder, or turn on the online lookup under Extras.</p>
        )}
      </section>
    </div>
  );
}
