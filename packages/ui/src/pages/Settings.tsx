import { useEffect, useState } from "react";
import { call } from "../api/backend";
import type { AutoUpdate, Config } from "../api/types";
import { useLoad } from "../hooks";
import { Button, Card, ErrorNote, Loading, PageHeader, useAction } from "../ui/components";
import { useToast } from "../ui/toast";

export function Settings() {
  const toast = useToast();
  const loaded = useLoad(() => call<Config>("config.get"));
  const { busy, run } = useAction(toast.error);
  const [draft, setDraft] = useState<Config | null>(null);
  useEffect(() => setDraft(loaded.data), [loaded.data]);

  if (loaded.error) return <ErrorNote error={loaded.error} retry={loaded.reload} />;
  if (!draft) return <Loading />;

  const dirty = JSON.stringify(draft) !== JSON.stringify(loaded.data);
  const field = "rounded-md border border-zinc-300 bg-white px-2 py-1.5 text-sm dark:border-zinc-700 dark:bg-zinc-950";

  return (
    <>
      <PageHeader title="Settings" />
      <Card className="max-w-xl space-y-5">
        <div>
          <label htmlFor="auto-update" className="mb-1 block text-sm font-medium">Automatic updates for plugins and themes</label>
          <select
            id="auto-update"
            className={field}
            value={draft.plugins.auto_update}
            onChange={(e) => setDraft({ ...draft, plugins: { ...draft.plugins, auto_update: e.target.value as AutoUpdate } })}
          >
            <option value="off">Off – only tell me</option>
            <option value="safe_only">Only clean updates without new permissions</option>
            <option value="all">All updates without new permissions</option>
          </select>
          <p className="mt-1 text-xs text-zinc-500">Updates that ask for new permissions always need your confirmation.</p>
        </div>

        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={draft.patch.auto_repatch} onChange={(e) => setDraft({ ...draft, patch: { ...draft.patch, auto_repatch: e.target.checked } })} />
          Patch Spotify again automatically after it updates
        </label>

        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={draft.general.check_app_updates} onChange={(e) => setDraft({ ...draft, general: { ...draft.general, check_app_updates: e.target.checked } })} />
          Check for Ghostify updates
        </label>

        <div>
          <label htmlFor="crash" className="mb-1 block text-sm font-medium">Disable a plugin after this many crashes</label>
          <input id="crash" type="number" min={1} className={`${field} w-24`} value={draft.plugins.crash_threshold}
            onChange={(e) => setDraft({ ...draft, plugins: { ...draft.plugins, crash_threshold: Number(e.target.value) } })} />
        </div>

        <div>
          <label htmlFor="refresh" className="mb-1 block text-sm font-medium">Marketplace refresh interval (seconds)</label>
          <input id="refresh" type="number" min={60} className={`${field} w-32`} value={draft.marketplace.refresh_interval_secs}
            onChange={(e) => setDraft({ ...draft, marketplace: { ...draft.marketplace, refresh_interval_secs: Number(e.target.value) } })} />
        </div>

        <div className="flex items-center gap-3">
          <Button
            variant="primary"
            disabled={busy || !dirty}
            onClick={() =>
              run(async () => {
                await call("config.save", { config: draft });
                toast.info("Saved. Restart Ghostify to apply the changes.");
                loaded.reload();
              })
            }
          >
            Save
          </Button>
          {dirty && <Button disabled={busy} onClick={() => setDraft(loaded.data)}>Discard</Button>}
        </div>
      </Card>
    </>
  );
}
