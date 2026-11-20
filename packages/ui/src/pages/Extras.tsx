import { useEffect, useState } from "react";
import { call, toggleMiniPlayer } from "../api/backend";
import type { AppInfo, Config, Hotkey } from "../api/types";
import { useLoad } from "../hooks";
import { Button, Card, ErrorNote, Loading, PageHeader, Switch, useAction } from "../ui/components";
import { useToast } from "../ui/toast";

const actionLabels: Record<string, string> = {
  play_pause: "Play / pause",
  next: "Next track",
  previous: "Previous track",
  toggle_mini_player: "Show / hide mini player",
};

function HotkeyRow({ hotkey, onChanged }: { hotkey: Hotkey; onChanged: () => void }) {
  const toast = useToast();
  const { busy, run } = useAction(toast.error);
  const [text, setText] = useState(hotkey.accelerator);
  useEffect(() => setText(hotkey.accelerator), [hotkey.accelerator]);
  const label = actionLabels[hotkey.action] ?? hotkey.action;
  const save = (accelerator: string, enabled: boolean) =>
    run(async () => {
      await call("hotkeys.set", { action: hotkey.action, accelerator, enabled });
      onChanged();
    });
  return (
    <li className="flex flex-wrap items-center gap-3 py-2">
      <span className="w-48 text-sm">{label}</span>
      <input
        aria-label={`Shortcut for ${label}`}
        value={text}
        onChange={(e) => setText(e.target.value)}
        className="w-64 rounded-md border border-zinc-300 bg-white px-2 py-1.5 text-sm dark:border-zinc-700 dark:bg-zinc-950"
      />
      <Button disabled={busy || text.trim() === hotkey.accelerator} onClick={() => save(text.trim(), hotkey.enabled)} aria-label={`Save shortcut for ${label}`}>Save</Button>
      <Button disabled={busy} onClick={() => run(async () => { await call("hotkeys.reset", { action: hotkey.action }); onChanged(); })} aria-label={`Reset shortcut for ${label}`}>Reset</Button>
      <Switch label={`Enable shortcut for ${label}`} checked={hotkey.enabled} onChange={(on) => save(hotkey.accelerator, on)} />
    </li>
  );
}

export function Extras() {
  const toast = useToast();
  const hotkeys = useLoad(() => call<Hotkey[]>("hotkeys.list"));
  const config = useLoad(() => call<Config>("config.get"));
  const info = useLoad(() => call<AppInfo>("app.info"));
  const { busy, run } = useAction(toast.error);
  const [discordId, setDiscordId] = useState("");
  useEffect(() => setDiscordId(config.data?.extras.discord_client_id ?? ""), [config.data]);

  const saveExtras = (change: Partial<Config["extras"]>) =>
    run(async () => {
      if (!config.data) return;
      await call("config.save", { config: { ...config.data, extras: { ...config.data.extras, ...change } } });
      toast.info("Saved. Restart Ghostify to apply the changes.");
      config.reload();
    });

  return (
    <>
      <PageHeader title="Extras">
        <Button variant="primary" onClick={() => toggleMiniPlayer().catch(toast.error)}>Mini player</Button>
      </PageHeader>

      <Card className="mb-5">
        <h2 className="mb-1 font-semibold">Keyboard shortcuts</h2>
        <p className="mb-2 text-xs text-zinc-500">
          They work in every program. Write them like <code>Ctrl+Alt+M</code>; media keys are <code>MediaPlayPause</code>, <code>MediaNextTrack</code> and <code>MediaPreviousTrack</code>.
        </p>
        {hotkeys.loading && !hotkeys.data && <Loading />}
        {hotkeys.error && <ErrorNote error={hotkeys.error} retry={hotkeys.reload} />}
        <ul className="divide-y divide-zinc-200 dark:divide-zinc-800">
          {hotkeys.data?.map((hotkey) => <HotkeyRow key={hotkey.action} hotkey={hotkey} onChanged={hotkeys.reload} />)}
        </ul>
      </Card>

      {config.error && <ErrorNote error={config.error} retry={config.reload} />}
      {config.data && (
        <>
          <Card className="mb-5">
            <div className="flex items-center justify-between gap-3">
              <div>
                <h2 className="font-semibold">Discord status</h2>
                <p className="text-xs text-zinc-500">Shows the song you listen to in Discord. Needs the Discord app on this computer.</p>
              </div>
              <Switch label="Show my music in Discord" checked={config.data.extras.discord_enabled} onChange={(on) => saveExtras({ discord_enabled: on })} />
            </div>
            <div className="mt-3 flex flex-wrap items-end gap-2">
              <div>
                <label htmlFor="discord-id" className="mb-1 block text-sm font-medium">Discord application ID</label>
                <input
                  id="discord-id"
                  inputMode="numeric"
                  value={discordId}
                  onChange={(e) => setDiscordId(e.target.value.trim())}
                  className="w-72 rounded-md border border-zinc-300 bg-white px-2 py-1.5 text-sm dark:border-zinc-700 dark:bg-zinc-950"
                />
              </div>
              <Button disabled={busy || discordId === config.data.extras.discord_client_id} onClick={() => saveExtras({ discord_client_id: discordId })}>Save ID</Button>
            </div>
            <p className="mt-2 text-xs text-zinc-500">Create a free application at discord.com/developers and paste its Application ID. Its name is what Discord shows as “Listening to …”.</p>
          </Card>

          <Card>
            <div className="flex items-center justify-between gap-3">
              <div>
                <h2 className="font-semibold">Lyrics</h2>
                <p className="text-xs text-zinc-500">
                  Lyrics are read from <code>.lrc</code> or <code>.txt</code> files named “Artist - Title” in your lyrics folder.
                </p>
              </div>
            </div>
            {info.data && <p className="mt-2 text-sm break-all">Folder: <code>{info.data.lyrics_dir}</code></p>}
            <label className="mt-3 flex items-start gap-2 text-sm">
              <input type="checkbox" checked={config.data.extras.lyrics_online} disabled={busy} onChange={(e) => saveExtras({ lyrics_online: e.target.checked })} />
              <span>
                Also look lyrics up on lrclib.net
                <span className="block text-xs text-zinc-500">Sends artist, title, album and length of the song to that website. Off by default.</span>
              </span>
            </label>
          </Card>
        </>
      )}
    </>
  );
}
