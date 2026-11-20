import { useEffect, useState } from "react";
import { call, toggleMiniPlayer } from "../api/backend";
import type { PlayerAction, PlayerSnapshot } from "../api/types";
import { Icon } from "./brand";
import { useToast } from "./toast";

const POLL_MS = 1000;

function clock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

/** The bar at the bottom of the window: what is playing, with the basic controls. */
export function PlayerBar() {
  const toast = useToast();
  const [snapshot, setSnapshot] = useState<PlayerSnapshot | null>(null);

  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const next = await call<PlayerSnapshot>("player.state");
        if (alive) setSnapshot(next);
      } catch {
        // Not connected to Spotify yet; the bar simply stays idle.
        if (alive) setSnapshot(null);
      }
    };
    void tick();
    const timer = window.setInterval(tick, POLL_MS);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);

  const send = async (action: PlayerAction) => {
    try {
      await call("player.command", { action });
      setSnapshot(await call<PlayerSnapshot>("player.state"));
    } catch (error) {
      toast.error(error);
    }
  };

  const track = snapshot?.track ?? null;
  const duration = track?.duration_ms ?? 0;
  const position = snapshot?.position_ms ?? 0;
  const percent = duration > 0 ? Math.min(100, (position / duration) * 100) : 0;
  const control = "rounded-full p-2 text-zinc-300 transition-colors hover:text-white disabled:opacity-40";

  return (
    <footer aria-label="Player" className="grid h-20 shrink-0 grid-cols-[1fr_minmax(0,2fr)_1fr] items-center gap-4 border-t border-white/10 bg-zinc-950 px-4">
      <div className="flex min-w-0 items-center gap-3">
        {track?.artwork ? (
          <img src={track.artwork} alt="" referrerPolicy="no-referrer" className="h-12 w-12 rounded-md object-cover grayscale" />
        ) : (
          <div className="h-12 w-12 shrink-0 rounded-md border border-white/10 bg-zinc-900" aria-hidden />
        )}
        <div className="min-w-0">
          <p className="truncate text-sm font-semibold uppercase tracking-wide" title={track?.title}>
            {track ? track.title : "Nothing playing"}
          </p>
          <p className="truncate text-xs text-zinc-400">{track ? track.artist : "Start a song in Spotify"}</p>
        </div>
      </div>

      <div className="flex flex-col items-center gap-1">
        <div className="flex items-center gap-3">
          <button type="button" aria-label="Previous track" disabled={!track} onClick={() => send("previous")} className={control}>
            <Icon name="prev" />
          </button>
          <button
            type="button"
            aria-label={snapshot?.playing ? "Pause" : "Play"}
            disabled={!track}
            onClick={() => send("toggle")}
            className="rounded-full bg-white p-2.5 text-black shadow-[0_0_22px_rgba(255,255,255,0.25)] transition-transform hover:scale-105 disabled:opacity-40"
          >
            <Icon name={snapshot?.playing ? "pause" : "play"} />
          </button>
          <button type="button" aria-label="Next track" disabled={!track} onClick={() => send("next")} className={control}>
            <Icon name="next" />
          </button>
        </div>
        <div className="flex w-full items-center gap-2 text-xs text-zinc-400">
          <span className="w-9 text-right tabular-nums">{clock(position)}</span>
          <div className="h-1 flex-1 overflow-hidden rounded-full bg-white/15">
            <div
              className="h-full rounded-full bg-white"
              style={{ width: `${percent}%` }}
              role="progressbar"
              aria-label="Progress"
              aria-valuenow={Math.round(percent)}
              aria-valuemin={0}
              aria-valuemax={100}
            />
          </div>
          <span className="w-9 tabular-nums">{duration > 0 ? clock(duration) : ""}</span>
        </div>
      </div>

      <div className="flex justify-end">
        <button
          type="button"
          aria-label="Open mini player"
          onClick={() => toggleMiniPlayer().catch((error) => toast.error(error))}
          className={control}
        >
          <Icon name="mini" />
        </button>
      </div>
    </footer>
  );
}
