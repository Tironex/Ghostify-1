import { useState } from "react";
import { BackendError, call } from "../api/backend";
import type { PluginInfo, PluginInstallResult, ScanVerdict } from "../api/types";
import { useLoad } from "../hooks";
import { Badge, Button, Card, Dialog, Empty, ErrorNote, Loading, PageHeader, PathField, Switch, permissionLabels, useAction } from "../ui/components";
import { useToast } from "../ui/toast";

const verdictTone: Record<ScanVerdict, "good" | "warn" | "bad"> = { clean: "good", suspicious: "warn", blocked: "bad" };

export function Plugins() {
  const toast = useToast();
  const plugins = useLoad(() => call<PluginInfo[]>("plugin.list"));
  const { busy, run } = useAction(toast.error);
  const [path, setPath] = useState("");
  const [replace, setReplace] = useState(false);
  const [removing, setRemoving] = useState<PluginInfo | null>(null);
  const [blocked, setBlocked] = useState<string | null>(null);

  const install = (allowBlocked: boolean) =>
    run(async () => {
      try {
        const result = await call<PluginInstallResult>("plugin.install", { path: path.trim(), replace, allow_blocked: allowBlocked });
        setPath("");
        setBlocked(null);
        const warnings = result.report.findings.filter((f) => f.severity !== "info").length;
        toast.info(`Installed ${result.info.name}${warnings ? ` – the scan reported ${warnings} warning${warnings === 1 ? "" : "s"}` : ""}. Review its permissions below.`);
        plugins.reload();
      } catch (error) {
        // The scanner refused the plugin: the user may overrule it after seeing why.
        if (error instanceof BackendError && error.code === "permission_denied" && !allowBlocked) setBlocked(error.message);
        else throw error;
      }
    });

  const change = (work: () => Promise<unknown>) =>
    run(async () => {
      await work();
      plugins.reload();
    });

  return (
    <>
      <PageHeader title="Plugins" />
      <Card className="mb-5">
        <h2 className="mb-1 font-semibold">Install a plugin</h2>
        <p className="mb-3 text-xs text-zinc-500">Every plugin is scanned first and starts without any permission.</p>
        <PathField label="Folder or .zip file" value={path} onChange={setPath} />
        <div className="mt-3 flex items-center justify-between">
          <label className="flex items-center gap-2 text-sm">
            <input type="checkbox" checked={replace} onChange={(e) => setReplace(e.target.checked)} /> Replace if already installed
          </label>
          <Button variant="primary" disabled={busy || !path.trim()} onClick={() => install(false)}>Install</Button>
        </div>
      </Card>

      {plugins.loading && !plugins.data && <Loading />}
      {plugins.error && <ErrorNote error={plugins.error} retry={plugins.reload} />}
      {plugins.data?.length === 0 && <Empty>No plugins installed.</Empty>}
      <div className="space-y-3">
        {plugins.data?.map((plugin) => (
          <Card key={plugin.id}>
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <p className="flex flex-wrap items-center gap-2 font-medium">
                  {plugin.name}
                  {plugin.scan && <Badge tone={verdictTone[plugin.scan.verdict]}>Scan: {plugin.scan.verdict}</Badge>}
                  {plugin.runtime === "legacy" && <Badge tone="warn">Legacy mode – not isolated</Badge>}
                  {plugin.state === "crashed" && <Badge tone="bad">Crashed</Badge>}
                </p>
                <p className="text-xs text-zinc-500">{plugin.id} · {plugin.version} · {plugin.source}</p>
              </div>
              <Switch label={`Enable ${plugin.name}`} checked={plugin.enabled} onChange={(on) => change(() => call(on ? "plugin.enable" : "plugin.disable", { id: plugin.id }))} />
            </div>

            {plugin.requested.length > 0 ? (
              <fieldset className="mt-3">
                <legend className="mb-1 text-xs font-medium text-zinc-500">Permissions</legend>
                {plugin.requested.map((permission) => (
                  <label key={permission} className="flex items-center gap-2 py-0.5 text-sm">
                    <input
                      type="checkbox"
                      checked={plugin.granted.includes(permission)}
                      disabled={busy}
                      onChange={(e) =>
                        change(() => call(e.target.checked ? "plugin.grant" : "plugin.revoke", { id: plugin.id, permission }))
                      }
                    />
                    <span>{permissionLabels[permission] ?? permission}</span>
                    <code className="text-xs text-zinc-500">{permission}</code>
                  </label>
                ))}
              </fieldset>
            ) : (
              <p className="mt-3 text-xs text-zinc-500">Asks for no permissions.</p>
            )}
            {plugin.scan && plugin.scan.findings.length > 0 && (
              <details className="mt-3 text-xs">
                <summary className="cursor-pointer text-zinc-500">{plugin.scan.findings.length} scan finding{plugin.scan.findings.length === 1 ? "" : "s"}</summary>
                <ul className="mt-1 list-disc pl-5">
                  {plugin.scan.findings.map((finding, i) => <li key={i}>{finding}</li>)}
                </ul>
              </details>
            )}
            <div className="mt-3 flex justify-end">
              <Button disabled={busy} onClick={() => setRemoving(plugin)}>Remove</Button>
            </div>
          </Card>
        ))}
      </div>

      {removing && (
        <Dialog
          title={`Remove ${removing.name}?`}
          onClose={() => setRemoving(null)}
          actions={
            <Button variant="danger" onClick={() => { const p = removing; setRemoving(null); change(async () => { await call("plugin.remove", { id: p.id }); toast.info(`Removed ${p.name}`); }); }}>
              Remove
            </Button>
          }
        >
          <p>The plugin and its stored settings are deleted.</p>
        </Dialog>
      )}
      {blocked !== null && (
        <Dialog
          title="The scan blocked this plugin"
          onClose={() => setBlocked(null)}
          actions={<Button variant="danger" onClick={() => install(true)}>Install anyway</Button>}
        >
          <p>{blocked}</p>
          <p className="text-red-700 dark:text-red-300">Only continue if you wrote this plugin or fully trust its author.</p>
        </Dialog>
      )}
    </>
  );
}
