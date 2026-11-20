import { call } from "../api/backend";
import type { AppInfo, DiskState, PatchOutcome, PatchReport, RepairResult, StatusReport } from "../api/types";
import { useLoad } from "../hooks";
import { Crown, Icon, Torii } from "../ui/brand";
import { useNavigate } from "../ui/nav";
import { Badge, Button, Card, Empty, ErrorNote, Loading, PageHeader, useAction } from "../ui/components";
import { useToast } from "../ui/toast";

const outcomeText: Record<PatchOutcome, string> = {
  already_healthy: "Already patched and healthy",
  patched: "Spotify is patched",
  repatched: "Spotify was updated and patched again",
  upgraded: "Patch upgraded",
  restored: "Original Spotify restored",
  nothing_to_restore: "Spotify was not patched",
};

function DiskBadge({ disk }: { disk: DiskState }) {
  switch (disk.state) {
    case "patched":
      return <Badge tone="good">Patched</Badge>;
    case "unpatched":
      return <Badge>Not patched</Badge>;
    case "missing":
      return <Badge tone="bad">Bundle missing</Badge>;
    case "unreadable":
      return <Badge tone="bad">Unreadable: {disk.reason}</Badge>;
  }
}

const tiles = [
  { page: "themes", label: "Choose a theme", icon: "palette" },
  { page: "plugins", label: "Manage plugins", icon: "plug" },
  { page: "marketplace", label: "Open the marketplace", icon: "store" },
  { page: "extras", label: "Lyrics and hotkeys", icon: "spark" },
] as const;

/** Banner at the top of the dashboard: moon, torii gate and shortcuts to the main pages. */
function Hero() {
  const navigate = useNavigate();
  return (
    <section aria-label="Welcome" className="ghost-hero relative mb-5 overflow-hidden rounded-2xl border border-white/10">
      <div className="ghost-moon absolute top-4 right-28 h-44 w-44 rounded-full opacity-90" aria-hidden />
      <Torii className="absolute right-8 bottom-28 h-28 text-zinc-950 drop-shadow-[0_0_1px_rgba(255,255,255,0.6)]" />
      <div className="relative z-10 px-8 pt-8 pb-5">
        <Crown className="mb-2 h-7 w-10 text-zinc-200" />
        <p className="ghost-wordmark text-5xl leading-none text-white sm:text-6xl">GHOSTIFY</p>
        <p className="mt-3 text-xs tracking-[0.4em] text-zinc-400">MUSIC · THEMES · YOUR SOUND</p>
        <p className="mt-8 text-2xl font-bold">Welcome back</p>
        <p className="text-sm text-zinc-400">What are we changing today?</p>
        <div className="mt-5 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {tiles.map(({ page, label, icon }) => (
            <button
              key={page}
              type="button"
              onClick={() => navigate(page)}
              className="ghost-tile flex items-center gap-3 rounded-xl border border-white/10 px-4 py-4 text-left text-sm font-semibold transition-colors hover:border-white/30"
            >
              <Icon name={icon} />
              {label}
            </button>
          ))}
        </div>
      </div>
    </section>
  );
}

export function Dashboard() {
  const toast = useToast();
  const info = useLoad(() => call<AppInfo>("app.info"));
  const status = useLoad(() => call<StatusReport[]>("patch.status"));
  const { busy, run } = useAction(toast.error);

  const act = (method: "patch.apply" | "patch.repair" | "patch.restore", install: number) =>
    run(async () => {
      const entries = await call<(PatchReport | RepairResult)[]>(method, { install });
      for (const entry of entries) {
        const report = "outcome" in entry ? entry : entry.report;
        const error = "error" in entry ? entry.error : undefined;
        if (error) toast.error(error);
        else if (report) toast.info(outcomeText[report.outcome]);
      }
      status.reload();
    });

  return (
    <>
      <PageHeader title="Dashboard">
        <Button onClick={() => { info.reload(); status.reload(); }}>Refresh</Button>
      </PageHeader>

      <Hero />

      <div className="mb-5 grid gap-4 sm:grid-cols-3">
        <Card>
          <p className="text-xs text-zinc-500">Ghostify</p>
          <p className="text-lg font-semibold">{info.data ? `Version ${info.data.version}` : "…"}</p>
        </Card>
        <Card>
          <p className="text-xs text-zinc-500">Connection to Spotify</p>
          <p className="text-lg font-semibold">
            {info.data ? (info.data.connected_loaders > 0 ? "Connected" : "Waiting for Spotify") : "…"}
          </p>
          {info.data && <p className="text-xs text-zinc-500">Local port {info.data.bridge_port}</p>}
        </Card>
        <Card>
          <p className="text-xs text-zinc-500">Data folder</p>
          <p className="truncate text-sm" title={info.data?.data_dir}>{info.data?.data_dir ?? "…"}</p>
        </Card>
      </div>
      {info.error && <ErrorNote error={info.error} retry={info.reload} />}

      <h2 className="mb-3 text-lg font-semibold">Spotify installations</h2>
      {status.loading && !status.data && <Loading />}
      {status.error && <ErrorNote error={status.error} retry={status.reload} />}
      {status.data?.length === 0 && (
        <Empty>
          No Spotify Desktop found. Install the version from spotify.com; the Microsoft Store and Snap versions cannot be patched.
        </Empty>
      )}
      <div className="space-y-3">
        {status.data?.map(({ install, disk, backups }) => (
          <Card key={install.id}>
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div className="min-w-0">
                <p className="flex items-center gap-2 font-medium">
                  Spotify {install.version ?? "(unknown version)"} <DiskBadge disk={disk} />
                </p>
                <p className="truncate text-xs text-zinc-500" title={install.root_path}>{install.root_path}</p>
                <p className="text-xs text-zinc-500">{backups} backup{backups === 1 ? "" : "s"}</p>
              </div>
              <div className="flex gap-2">
                <Button variant="primary" disabled={busy} onClick={() => act("patch.apply", install.id)}>
                  {disk.state === "patched" ? "Re-apply" : "Patch"}
                </Button>
                <Button disabled={busy || disk.state !== "patched"} onClick={() => act("patch.repair", install.id)}>
                  Repair
                </Button>
                <Button disabled={busy || disk.state !== "patched"} onClick={() => act("patch.restore", install.id)}>
                  Restore
                </Button>
              </div>
            </div>
          </Card>
        ))}
      </div>
    </>
  );
}
