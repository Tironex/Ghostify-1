import { useState } from "react";
import { call } from "../api/backend";
import type { ThemeInfo, ThemeMode } from "../api/types";
import { useLoad } from "../hooks";
import { Badge, Button, Card, Dialog, Empty, ErrorNote, Loading, PageHeader, PathField, useAction } from "../ui/components";
import { useToast } from "../ui/toast";

const modes: ThemeMode[] = ["system", "dark", "light"];

export function Themes() {
  const toast = useToast();
  const themes = useLoad(() => call<ThemeInfo[]>("theme.list"));
  const mode = useLoad(() => call<ThemeMode>("theme.mode"));
  const { busy, run } = useAction(toast.error);
  const [path, setPath] = useState("");
  const [replace, setReplace] = useState(false);
  const [removing, setRemoving] = useState<ThemeInfo | null>(null);
  const [exporting, setExporting] = useState<ThemeInfo | null>(null);
  const [exportPath, setExportPath] = useState("");

  const done = (text: string) => {
    toast.info(text);
    themes.reload();
  };

  return (
    <>
      <PageHeader title="Themes">
        <div role="group" aria-label="Appearance" className="flex overflow-hidden rounded-md border border-zinc-300 dark:border-zinc-700">
          {modes.map((m) => (
            <button
              key={m}
              type="button"
              aria-pressed={mode.data === m}
              onClick={() => run(async () => { await call("theme.set_mode", { mode: m }); mode.reload(); })}
              className={`px-3 py-1.5 text-sm capitalize ${mode.data === m ? "bg-white text-black" : "bg-white dark:bg-zinc-900"}`}
            >
              {m}
            </button>
          ))}
        </div>
      </PageHeader>

      <Card className="mb-5">
        <h2 className="mb-3 font-semibold">Install a theme</h2>
        <PathField label="Folder or .zip file" value={path} onChange={setPath} placeholder="Spicetify themes work too" />
        <div className="mt-3 flex items-center justify-between">
          <label className="flex items-center gap-2 text-sm">
            <input type="checkbox" checked={replace} onChange={(e) => setReplace(e.target.checked)} /> Replace if already installed
          </label>
          <Button
            variant="primary"
            disabled={busy || !path.trim()}
            onClick={() =>
              run(async () => {
                const result = await call<{ info: ThemeInfo; notes: string[] }>("theme.install", { path: path.trim(), replace });
                setPath("");
                done(`Installed ${result.info.name}${result.notes.length ? ` (${result.notes.length} notes)` : ""}`);
              })
            }
          >
            Install
          </Button>
        </div>
      </Card>

      {themes.loading && !themes.data && <Loading />}
      {themes.error && <ErrorNote error={themes.error} retry={themes.reload} />}
      {themes.data?.length === 0 && <Empty>No themes yet. Install one above or find one in the marketplace.</Empty>}
      <div className="grid gap-3 md:grid-cols-2">
        {themes.data?.map((theme) => (
          <Card key={theme.id}>
            <div className="flex items-start justify-between gap-2">
              <div className="min-w-0">
                <p className="truncate font-medium">{theme.name}</p>
                <p className="text-xs text-zinc-500">{theme.id} · {theme.version} · {theme.source}</p>
              </div>
              {theme.active && <Badge tone="accent">Active</Badge>}
            </div>
            {theme.color_schemes.length > 1 && (
              <label className="mt-3 flex items-center gap-2 text-sm">
                Color scheme
                <select
                  value={theme.color_scheme}
                  disabled={busy}
                  onChange={(e) => run(async () => { await call("theme.activate", { id: theme.id, scheme: e.target.value }); themes.reload(); })}
                  className="rounded-md border border-zinc-300 bg-white px-2 py-1 dark:border-zinc-700 dark:bg-zinc-950"
                >
                  {theme.color_schemes.map((s) => <option key={s}>{s}</option>)}
                </select>
              </label>
            )}
            <div className="mt-4 flex flex-wrap gap-2">
              {theme.active ? (
                <Button disabled={busy} onClick={() => run(async () => { await call("theme.deactivate"); done("Theme turned off"); })}>
                  Turn off
                </Button>
              ) : (
                <Button variant="primary" disabled={busy} onClick={() => run(async () => { await call("theme.activate", { id: theme.id }); done(`${theme.name} is active`); })}>
                  Activate
                </Button>
              )}
              <Button disabled={busy} onClick={() => { setExporting(theme); setExportPath(""); }}>Export</Button>
              <Button disabled={busy} onClick={() => setRemoving(theme)}>Remove</Button>
            </div>
          </Card>
        ))}
      </div>

      {removing && (
        <Dialog
          title={`Remove ${removing.name}?`}
          onClose={() => setRemoving(null)}
          actions={
            <Button
              variant="danger"
              onClick={() => {
                const theme = removing;
                setRemoving(null);
                run(async () => { await call("theme.remove", { id: theme.id }); done(`Removed ${theme.name}`); });
              }}
            >
              Remove
            </Button>
          }
        >
          <p>The theme's files are deleted. Export it first if you want to keep a copy.</p>
        </Dialog>
      )}
      {exporting && (
        <Dialog
          title={`Export ${exporting.name}`}
          onClose={() => setExporting(null)}
          actions={
            <Button
              variant="primary"
              disabled={!exportPath.trim()}
              onClick={() => {
                const theme = exporting;
                setExporting(null);
                run(async () => { await call("theme.export", { id: theme.id, out: exportPath.trim() }); toast.info(`Exported ${theme.name}`); });
              }}
            >
              Export
            </Button>
          }
        >
          <PathField label="Save as (.zip)" value={exportPath} onChange={setExportPath} />
        </Dialog>
      )}
    </>
  );
}
