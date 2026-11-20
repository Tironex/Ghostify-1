import { useState, type FormEvent } from "react";
import { BackendError, call } from "../api/backend";
import type { CategoryCount, Listing, PackageInfo, PluginInfo, RefreshOutcome, ThemeInfo, UpdateInfo } from "../api/types";
import { useLoad } from "../hooks";
import { Badge, Button, Card, Dialog, Empty, ErrorNote, PageHeader, permissionLabels, useAction } from "../ui/components";
import { useToast } from "../ui/toast";

type Kind = "all" | "plugin" | "theme";

type Pending =
  | { type: "install"; listing: Listing; permissions: string[]; size: number }
  | { type: "update"; update: UpdateInfo; name: string }
  | { type: "blocked"; listing: Listing; message: string };

function PermissionList({ permissions }: { permissions: string[] }) {
  if (permissions.length === 0) return <p>It asks for no permissions.</p>;
  return (
    <ul className="list-disc pl-5">
      {permissions.map((p) => <li key={p}>{permissionLabels[p] ?? p}</li>)}
    </ul>
  );
}

function Stars({ listing, onRate }: { listing: Listing; onRate: (score: number) => void }) {
  return (
    <div className="flex items-center gap-0.5" role="group" aria-label={`Rate ${listing.name}`}>
      {[1, 2, 3, 4, 5].map((n) => (
        <button
          key={n}
          type="button"
          aria-label={`Rate ${n} star${n === 1 ? "" : "s"}`}
          onClick={() => onRate(n)}
          className={`text-lg leading-none ${listing.rating_avg !== null && n <= Math.round(listing.rating_avg) ? "text-amber-500" : "text-zinc-300 dark:text-zinc-700"}`}
        >
          ★
        </button>
      ))}
      <span className="ml-1 text-xs text-zinc-500">{listing.rating_count > 0 ? `${listing.rating_avg?.toFixed(1)} (${listing.rating_count})` : "no ratings"}</span>
    </div>
  );
}

export function Marketplace() {
  const toast = useToast();
  const { busy, run } = useAction(toast.error);
  const [text, setText] = useState("");
  const [kind, setKind] = useState<Kind>("all");
  const [category, setCategory] = useState<string | null>(null);
  const [submitted, setSubmitted] = useState("");
  const [pending, setPending] = useState<Pending | null>(null);

  const categories = useLoad(() => call<CategoryCount[]>("market.categories"));
  const updates = useLoad(() => call<UpdateInfo[]>("market.updates"));
  const installed = useLoad(async () => {
    const [plugins, themes] = await Promise.all([call<PluginInfo[]>("plugin.list"), call<ThemeInfo[]>("theme.list")]);
    return new Set([...plugins.map((p) => p.id), ...themes.map((t) => t.id)]);
  });
  const results = useLoad(
    () =>
      call<Listing[]>("market.search", {
        text: submitted,
        kind: kind === "all" ? null : kind,
        category,
        limit: 60,
        offset: 0,
      }),
    [submitted, kind, category],
  );

  const reloadAll = () => {
    results.reload();
    updates.reload();
    installed.reload();
    categories.reload();
  };

  const search = (event: FormEvent) => {
    event.preventDefault();
    setSubmitted(text.trim());
  };

  const refresh = () =>
    run(async () => {
      const outcome = await call<RefreshOutcome>("market.refresh");
      toast.info(outcome.result === "applied" ? `Index updated: ${outcome.listings} listings` : "Index is up to date");
      reloadAll();
    });

  const startInstall = (listing: Listing) =>
    run(async () => {
      const pkg = await call<PackageInfo>("market.package", { id: listing.id });
      setPending({ type: "install", listing, permissions: pkg.permissions, size: pkg.size });
    });

  const confirmInstall = (listing: Listing, allowBlocked: boolean) => {
    setPending(null);
    return run(async () => {
      try {
        await call("market.install", { id: listing.id, allow_blocked: allowBlocked });
        toast.info(`Installed ${listing.name}${listing.kind === "plugin" ? ". Grant its permissions on the Plugins page." : ""}`);
        reloadAll();
      } catch (error) {
        if (error instanceof BackendError && error.code === "permission_denied" && error.message.includes("scan") && !allowBlocked) {
          setPending({ type: "blocked", listing, message: error.message });
        } else throw error;
      }
    });
  };

  const confirmUpdate = (update: UpdateInfo, allowNew: boolean) => {
    setPending(null);
    return run(async () => {
      await call("market.update", { id: update.listing_id, allow_new_permissions: allowNew });
      toast.info(`Updated to ${update.new_version}`);
      reloadAll();
    });
  };

  const updateFor = (id: string) => updates.data?.find((u) => u.listing_id === id);
  const nameOf = (id: string) => results.data?.find((l) => l.id === id)?.name ?? id;

  const startUpdate = (update: UpdateInfo) => {
    if (update.new_permissions.length > 0) setPending({ type: "update", update, name: nameOf(update.listing_id) });
    else confirmUpdate(update, false);
  };

  return (
    <>
      <PageHeader title="Marketplace">
        <Button disabled={busy} onClick={refresh}>Refresh index</Button>
      </PageHeader>

      {updates.data && updates.data.length > 0 && (
        <Card className="mb-5 border-zinc-300 dark:border-white/30">
          <h2 className="mb-2 font-semibold">{updates.data.length} update{updates.data.length === 1 ? "" : "s"} available</h2>
          <ul className="space-y-2">
            {updates.data.map((u) => (
              <li key={u.listing_id} className="flex items-center justify-between gap-2 text-sm">
                <span>
                  {nameOf(u.listing_id)} {u.current_version} → {u.new_version}
                  {u.new_permissions.length > 0 && <> <Badge tone="warn">new permissions</Badge></>}
                </span>
                <Button variant="primary" disabled={busy} aria-label={`Update ${nameOf(u.listing_id)}`} onClick={() => startUpdate(u)}>Update</Button>
              </li>
            ))}
          </ul>
        </Card>
      )}

      <form onSubmit={search} className="mb-4 flex flex-wrap gap-2" role="search">
        <input
          type="search"
          aria-label="Search the marketplace"
          placeholder="Search themes and plugins"
          value={text}
          onChange={(e) => setText(e.target.value)}
          className="min-w-0 flex-1 rounded-md border border-zinc-300 bg-white px-3 py-2 text-sm dark:border-zinc-700 dark:bg-zinc-900"
        />
        <select
          aria-label="Type"
          value={kind}
          onChange={(e) => setKind(e.target.value as Kind)}
          className="rounded-md border border-zinc-300 bg-white px-2 text-sm dark:border-zinc-700 dark:bg-zinc-900"
        >
          <option value="all">Everything</option>
          <option value="plugin">Plugins</option>
          <option value="theme">Themes</option>
        </select>
        <Button type="submit" variant="primary">Search</Button>
      </form>

      {categories.data && categories.data.length > 0 && (
        <div className="mb-4 flex flex-wrap gap-2" role="group" aria-label="Categories">
          <button type="button" aria-pressed={category === null} onClick={() => setCategory(null)}
            className={`rounded-full px-3 py-1 text-xs ${category === null ? "bg-white text-black" : "bg-zinc-200 dark:bg-zinc-800"}`}>All</button>
          {categories.data.map((c) => (
            <button key={c.category} type="button" aria-pressed={category === c.category} onClick={() => setCategory(c.category)}
              className={`rounded-full px-3 py-1 text-xs ${category === c.category ? "bg-white text-black" : "bg-zinc-200 dark:bg-zinc-800"}`}>
              {c.category} ({c.count})
            </button>
          ))}
        </div>
      )}

      {results.error && <ErrorNote error={results.error} retry={results.reload} />}
      {results.data?.length === 0 && (
        <Empty>Nothing found. If the index has never been loaded, press “Refresh index” (needs an internet connection).</Empty>
      )}
      <div className="grid gap-3 md:grid-cols-2">
        {results.data?.map((listing) => {
          const isInstalled = installed.data?.has(listing.id) ?? false;
          const update = updateFor(listing.id);
          return (
            <Card key={listing.id}>
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0">
                  <p className="truncate font-medium">{listing.name}</p>
                  <p className="text-xs text-zinc-500">{listing.author} · {listing.latest_version} · {listing.category}</p>
                </div>
                <Badge tone={listing.kind === "plugin" ? "accent" : "neutral"}>{listing.kind}</Badge>
              </div>
              <p className="mt-2 text-sm text-zinc-600 dark:text-zinc-400">{listing.description}</p>
              <div className="mt-3 flex items-center justify-between gap-2">
                <Stars listing={listing} onRate={(score) => run(async () => { await call("market.rate", { id: listing.id, score }); toast.info(`Rated ${listing.name} with ${score}`); })} />
                {update ? (
                  <Button variant="primary" disabled={busy} aria-label={`Update ${listing.name}`} onClick={() => startUpdate(update)}>Update</Button>
                ) : isInstalled ? (
                  <Badge tone="good">Installed</Badge>
                ) : (
                  <Button variant="primary" disabled={busy} aria-label={`Install ${listing.name}`} onClick={() => startInstall(listing)}>Install</Button>
                )}
              </div>
            </Card>
          );
        })}
      </div>

      {pending?.type === "install" && (
        <Dialog
          title={`Install ${pending.listing.name}?`}
          onClose={() => setPending(null)}
          actions={<Button variant="primary" onClick={() => confirmInstall(pending.listing, false)}>Install</Button>}
        >
          <p>Version {pending.listing.latest_version} by {pending.listing.author} · {(pending.size / 1024).toFixed(0)} KB. The download is checked against the signed index and scanned.</p>
          {pending.listing.kind === "plugin" && (
            <>
              <p className="font-medium">It will be able to ask for:</p>
              <PermissionList permissions={pending.permissions} />
              <p className="text-xs text-zinc-500">Nothing is granted until you allow it on the Plugins page.</p>
            </>
          )}
        </Dialog>
      )}
      {pending?.type === "update" && (
        <Dialog
          title={`Update ${pending.name}?`}
          onClose={() => setPending(null)}
          actions={<Button variant="primary" onClick={() => confirmUpdate(pending.update, true)}>Update</Button>}
        >
          <p>Version {pending.update.new_version} asks for permissions the installed version does not have:</p>
          <PermissionList permissions={pending.update.new_permissions} />
        </Dialog>
      )}
      {pending?.type === "blocked" && (
        <Dialog
          title="The scan blocked this package"
          onClose={() => setPending(null)}
          actions={<Button variant="danger" onClick={() => confirmInstall(pending.listing, true)}>Install anyway</Button>}
        >
          <p>{pending.message}</p>
          <p className="text-red-700 dark:text-red-300">Only continue if you fully trust the author.</p>
        </Dialog>
      )}
    </>
  );
}
