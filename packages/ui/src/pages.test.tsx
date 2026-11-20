import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "./App";
import { Dashboard } from "./pages/Dashboard";
import { Marketplace } from "./pages/Marketplace";
import { Plugins } from "./pages/Plugins";
import { Settings } from "./pages/Settings";
import { Themes } from "./pages/Themes";
import type { Config, Listing, PluginInfo, ThemeInfo } from "./api/types";
import { backend, fail, renderWithToasts } from "./test-utils";

const install = { id: 1, kind: "standard", root_path: "/opt/spotify/Apps", version: "1.2.60", last_seen_at: 0 };
const info = { version: "0.1.0", bridge_port: 49421, connected_loaders: 1, data_dir: "/home/me/.ghostify" };

describe("Dashboard", () => {
  it("shows the connection and installations and patches one", async () => {
    let patched = false;
    const rec = backend({
      "app.info": () => info,
      "patch.status": () => [{ install, state: null, disk: patched ? { state: "patched", ruleset_version: "builtin-2", spotify_version: "1.2.60" } : { state: "unpatched" }, backups: patched ? 1 : 0 }],
      "patch.apply": () => { patched = true; return [{ install_id: 1, outcome: "patched" }]; },
    });
    renderWithToasts(<Dashboard />);
    expect(await screen.findByText("Not patched")).toBeInTheDocument();
    expect(screen.getByText("Connected")).toBeInTheDocument();
    expect(screen.getByText("Local port 49421")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Repair" })).toBeDisabled();

    await userEvent.click(screen.getByRole("button", { name: "Patch" }));
    expect(await screen.findByText("Patched")).toBeInTheDocument();
    expect(await screen.findByText("Spotify is patched")).toBeInTheDocument();
    expect(rec.of("patch.apply")).toEqual([{ install: 1 }]);
    expect(screen.getByRole("button", { name: "Repair" })).toBeEnabled();
  });

  it("explains an empty system and reports repair errors", async () => {
    backend({ "app.info": () => info, "patch.status": () => [] });
    renderWithToasts(<Dashboard />);
    expect(await screen.findByText(/No Spotify Desktop found/)).toBeInTheDocument();
  });

  it("reports an error from the backend with a retry", async () => {
    backend({ "app.info": () => info, "patch.status": () => fail("database", "disk is full") });
    renderWithToasts(<Dashboard />);
    expect(await screen.findByRole("alert")).toHaveTextContent("disk is full");
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
  });

  it("shows repair failures as errors", async () => {
    backend({
      "app.info": () => info,
      "patch.status": () => [{ install, state: null, disk: { state: "patched", ruleset_version: "x", spotify_version: null }, backups: 1 }],
      "patch.repair": () => [{ install_id: 1, error: "permission denied: Apps" }],
    });
    renderWithToasts(<Dashboard />);
    await userEvent.click(await screen.findByRole("button", { name: "Repair" }));
    expect(await screen.findByText("permission denied: Apps")).toBeInTheDocument();
  });
});

const theme = (over: Partial<ThemeInfo> = {}): ThemeInfo => ({ id: "me/dark", name: "Dark", version: "1.0.0", source: "local", active: false, color_scheme: "dark", color_schemes: ["dark"], ...over });

describe("Themes", () => {
  it("activates, switches mode, installs and removes after confirming", async () => {
    let themes = [theme()];
    let mode = "system";
    const rec = backend({
      "theme.list": () => themes,
      "theme.mode": () => mode,
      "theme.set_mode": ({ mode: m }) => { mode = m; return null; },
      "theme.activate": ({ id }) => { themes = themes.map((t) => ({ ...t, active: t.id === id })); return themes[0]; },
      "theme.install": ({ path }) => { themes = [...themes, theme({ id: "me/new", name: "Fresh" })]; return { info: themes[1], notes: [], path }; },
      "theme.remove": ({ id }) => { themes = themes.filter((t) => t.id !== id); return null; },
    });
    renderWithToasts(<Themes />);
    await userEvent.click(await screen.findByRole("button", { name: "Activate" }));
    expect(await screen.findByText("Active")).toBeInTheDocument();
    expect(rec.of("theme.activate")).toEqual([{ id: "me/dark" }]);

    await userEvent.click(screen.getByRole("button", { name: "dark" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "dark" })).toHaveAttribute("aria-pressed", "true"));

    expect(screen.getByRole("button", { name: "Install" })).toBeDisabled();
    await userEvent.type(screen.getByLabelText("Folder or .zip file"), "/tmp/fresh");
    await userEvent.click(screen.getByRole("button", { name: "Install" }));
    expect(await screen.findByText("Fresh")).toBeInTheDocument();
    expect(rec.of("theme.install")).toEqual([{ path: "/tmp/fresh", replace: false }]);

    const card = screen.getByText("Fresh").closest("section") as HTMLElement;
    await userEvent.click(within(card).getByRole("button", { name: "Remove" }));
    expect(rec.of("theme.remove")).toEqual([]);
    await userEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(screen.queryByText("Fresh")).not.toBeInTheDocument());
    expect(rec.of("theme.remove")).toEqual([{ id: "me/new" }]);
  });

  it("changes the color scheme and exports", async () => {
    const rec = backend({
      "theme.list": () => [theme({ color_schemes: ["dark", "ocean"], active: true })],
      "theme.mode": () => "dark",
      "theme.activate": () => theme(),
      "theme.export": () => null,
    });
    renderWithToasts(<Themes />);
    await userEvent.selectOptions(await screen.findByLabelText("Color scheme"), "ocean");
    expect(rec.of("theme.activate")).toEqual([{ id: "me/dark", scheme: "ocean" }]);

    await userEvent.click(screen.getByRole("button", { name: "Export" }));
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByRole("button", { name: "Export" })).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText("Save as (.zip)"), "/tmp/dark.zip");
    await userEvent.click(within(dialog).getByRole("button", { name: "Export" }));
    await waitFor(() => expect(rec.of("theme.export")).toEqual([{ id: "me/dark", out: "/tmp/dark.zip" }]));
  });

  it("closes dialogs with Escape without acting", async () => {
    const rec = backend({ "theme.list": () => [theme()], "theme.mode": () => "system", "theme.remove": () => null });
    renderWithToasts(<Themes />);
    await userEvent.click(await screen.findByRole("button", { name: "Remove" }));
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(rec.of("theme.remove")).toEqual([]);
  });
});

const plugin = (over: Partial<PluginInfo> = {}): PluginInfo => ({
  id: "me/hello", name: "Hello", version: "1.0.0", source: "local", runtime: "sandboxed", enabled: false, state: "installed",
  requested: ["storage", "network"], granted: [], scan: { verdict: "clean", findings: [] }, ...over,
});

describe("Plugins", () => {
  it("grants and revokes permissions individually and toggles the plugin", async () => {
    let p = plugin();
    const rec = backend({
      "plugin.list": () => [p],
      "plugin.grant": ({ permission }) => { p = { ...p, granted: [...p.granted, permission] }; return p; },
      "plugin.revoke": ({ permission }) => { p = { ...p, granted: p.granted.filter((g) => g !== permission) }; return p; },
      "plugin.enable": () => { p = { ...p, enabled: true }; return p; },
    });
    renderWithToasts(<Plugins />);
    const box = await screen.findByRole("checkbox", { name: /Store its own settings/ });
    expect(box).not.toBeChecked();
    await userEvent.click(box);
    await waitFor(() => expect(screen.getByRole("checkbox", { name: /Store its own settings/ })).toBeChecked());
    expect(screen.getByRole("checkbox", { name: /Connect to the internet/ })).not.toBeChecked();
    await userEvent.click(screen.getByRole("checkbox", { name: /Store its own settings/ }));
    await waitFor(() => expect(screen.getByRole("checkbox", { name: /Store its own settings/ })).not.toBeChecked());
    expect(rec.of("plugin.grant")).toEqual([{ id: "me/hello", permission: "storage" }]);
    expect(rec.of("plugin.revoke")).toEqual([{ id: "me/hello", permission: "storage" }]);

    await userEvent.click(screen.getByRole("switch", { name: "Enable Hello" }));
    await waitFor(() => expect(screen.getByRole("switch", { name: "Enable Hello" })).toHaveAttribute("aria-checked", "true"));
  });

  it("offers an override only after the scanner blocked the plugin", async () => {
    const rec = backend({
      "plugin.list": () => [],
      "plugin.install": ({ allow_blocked }) => {
        if (!allow_blocked) fail("permission_denied", "permission denied: the scan blocked `me/bad` (eval)");
        return { info: plugin({ id: "me/bad", name: "Bad" }), report: { verdict: "blocked", findings: [] }, notes: [] };
      },
    });
    renderWithToasts(<Plugins />);
    await userEvent.type(await screen.findByLabelText("Folder or .zip file"), "/tmp/bad");
    await userEvent.click(screen.getByRole("button", { name: "Install" }));
    const dialog = await screen.findByRole("dialog", { name: "The scan blocked this plugin" });
    expect(dialog).toHaveTextContent("eval");
    await userEvent.click(within(dialog).getByRole("button", { name: "Install anyway" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(rec.of("plugin.install")).toEqual([
      { path: "/tmp/bad", replace: false, allow_blocked: false },
      { path: "/tmp/bad", replace: false, allow_blocked: true },
    ]);
  });

  it("shows other install errors as errors, not as an override offer", async () => {
    backend({ "plugin.list": () => [], "plugin.install": () => fail("invalid_input", "invalid input: plugin.json is missing") });
    renderWithToasts(<Plugins />);
    await userEvent.type(await screen.findByLabelText("Folder or .zip file"), "/tmp/x");
    await userEvent.click(screen.getByRole("button", { name: "Install" }));
    expect(await screen.findByText(/plugin.json is missing/)).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("flags legacy plugins and shows scan findings", async () => {
    backend({ "plugin.list": () => [plugin({ runtime: "legacy", scan: { verdict: "suspicious", findings: ["index.js:3 fetch of an unknown host"] } })] });
    renderWithToasts(<Plugins />);
    expect(await screen.findByText(/Legacy mode/)).toBeInTheDocument();
    expect(screen.getByText("Scan: suspicious")).toBeInTheDocument();
    expect(screen.getByText("index.js:3 fetch of an unknown host")).toBeInTheDocument();
  });
});

const listing = (over: Partial<Listing> = {}): Listing => ({
  id: "me/lyrics", kind: "plugin", name: "Lyrics", author: "me", repo: "me/lyrics", category: "lyrics", description: "Shows lyrics",
  tags: [], latest_version: "1.0.0", stars: 3, rating_avg: null, rating_count: 0, ...over,
});

describe("Marketplace", () => {
  const base = (extra: Record<string, (p: any) => unknown> = {}) =>
    backend({
      "market.categories": () => [{ category: "lyrics", count: 1 }],
      "market.updates": () => [],
      "plugin.list": () => [],
      "theme.list": () => [],
      "market.search": () => [listing()],
      ...extra,
    });

  it("searches with filters", async () => {
    const rec = base();
    renderWithToasts(<Marketplace />);
    expect(await screen.findByText("Lyrics")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Search the marketplace"), "lyr{enter}");
    await userEvent.selectOptions(screen.getByLabelText("Type"), "theme");
    await userEvent.click(screen.getByRole("button", { name: /lyrics \(1\)/ }));
    await waitFor(() => expect(rec.of("market.search").at(-1)).toEqual({ text: "lyr", kind: "theme", category: "lyrics", limit: 60, offset: 0 }));
  });

  it("shows permissions before installing and installs only after consent", async () => {
    const rec = base({
      "market.package": () => ({ listing_id: "me/lyrics", version: "1.0.0", download_url: "", sha256: "", size: 20480, permissions: ["network", "player.read"] }),
      "market.install": () => null,
    });
    renderWithToasts(<Marketplace />);
    await userEvent.click(await screen.findByRole("button", { name: "Install Lyrics" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("See what is playing");
    expect(dialog).toHaveTextContent("20 KB");
    expect(rec.of("market.install")).toEqual([]);
    await userEvent.click(within(dialog).getByRole("button", { name: "Install" }));
    await waitFor(() => expect(rec.of("market.install")).toEqual([{ id: "me/lyrics", allow_blocked: false }]));
  });

  it("does not install when the dialog is cancelled", async () => {
    const rec = base({ "market.package": () => ({ permissions: [], size: 1 }), "market.install": () => null });
    renderWithToasts(<Marketplace />);
    await userEvent.click(await screen.findByRole("button", { name: "Install Lyrics" }));
    await userEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Cancel" }));
    expect(rec.of("market.install")).toEqual([]);
  });

  it("marks installed items and asks again if the scan blocks the download", async () => {
    let allowed = false;
    const rec = base({
      "plugin.list": () => [plugin({ id: "me/other" })],
      "market.search": () => [listing(), listing({ id: "me/other", name: "Other" })],
      "market.package": () => ({ permissions: [], size: 1024 }),
      "market.install": ({ allow_blocked }) => {
        allowed = allow_blocked;
        if (!allow_blocked) fail("permission_denied", "permission denied: the scan of `me/lyrics` found a blocked pattern");
        return null;
      },
    });
    renderWithToasts(<Marketplace />);
    expect(await screen.findByText("Installed")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Install Lyrics" }));
    await userEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Install" }));
    const blocked = await screen.findByRole("dialog", { name: "The scan blocked this package" });
    await userEvent.click(within(blocked).getByRole("button", { name: "Install anyway" }));
    await waitFor(() => expect(allowed).toBe(true));
    expect(rec.of("market.install")).toHaveLength(2);
  });

  it("updates silently without new permissions and asks with them", async () => {
    const updates = [
      { listing_id: "me/lyrics", kind: "plugin", current_version: "1.0.0", new_version: "1.1.0", new_permissions: [] },
      { listing_id: "me/other", kind: "plugin", current_version: "1.0.0", new_version: "2.0.0", new_permissions: ["clipboard"] },
    ];
    const rec = base({
      "market.search": () => [listing(), listing({ id: "me/other", name: "Other" })],
      "market.updates": () => updates,
      "market.update": () => ({}),
    });
    renderWithToasts(<Marketplace />);
    expect(await screen.findByText("2 updates available")).toBeInTheDocument();
    const banner = screen.getByText("2 updates available").closest("section") as HTMLElement;

    await userEvent.click(within(banner).getByRole("button", { name: "Update Lyrics" }));
    await waitFor(() => expect(rec.of("market.update")).toEqual([{ id: "me/lyrics", allow_new_permissions: false }]));

    await userEvent.click(within(banner).getByRole("button", { name: "Update Other" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("Use the clipboard");
    expect(rec.of("market.update")).toHaveLength(1);
    await userEvent.click(within(dialog).getByRole("button", { name: "Update" }));
    await waitFor(() => expect(rec.of("market.update")).toHaveLength(2));
    expect(rec.of("market.update")[1]).toEqual({ id: "me/other", allow_new_permissions: true });
  });

  it("rates and refreshes the index", async () => {
    const rec = base({
      "market.rate": () => ({}),
      "market.refresh": () => ({ result: "applied", sequence: 3, listings: 12 }),
    });
    renderWithToasts(<Marketplace />);
    await userEvent.click(await screen.findByRole("button", { name: "Rate 4 stars" }));
    await waitFor(() => expect(rec.of("market.rate")).toEqual([{ id: "me/lyrics", score: 4 }]));
    await userEvent.click(screen.getByRole("button", { name: "Refresh index" }));
    expect(await screen.findByText("Index updated: 12 listings")).toBeInTheDocument();
  });

  it("explains an empty index", async () => {
    base({ "market.search": () => [], "market.categories": () => [] });
    renderWithToasts(<Marketplace />);
    expect(await screen.findByText(/Refresh index/, { selector: "p" })).toBeInTheDocument();
  });

  it("shows when the index cannot be refreshed", async () => {
    base({ "market.refresh": () => fail("io", "i/o error: request failed") });
    renderWithToasts(<Marketplace />);
    await userEvent.click(await screen.findByRole("button", { name: "Refresh index" }));
    expect(await screen.findByText(/request failed/)).toBeInTheDocument();
  });
});

const config: Config = {
  general: { check_app_updates: true, language: "en" },
  patch: { auto_repatch: true, backups_to_keep: 3 },
  plugins: { auto_update: "safe_only", crash_threshold: 3 },
  marketplace: { index_url: "https://ghostify.github.io/registry/index.json", refresh_interval_secs: 21600 },
  bridge: { port: 0 },
  extras: { discord_enabled: false, discord_client_id: "", lyrics_online: false },
};

describe("Settings", () => {
  it("saves only after a change and keeps untouched settings", async () => {
    const rec = backend({ "config.get": () => config, "config.save": () => ({ restart_required: true }) });
    renderWithToasts(<Settings />);
    const save = await screen.findByRole("button", { name: "Save" });
    expect(save).toBeDisabled();
    await userEvent.selectOptions(screen.getByLabelText(/Automatic updates/), "off");
    expect(save).toBeEnabled();
    await userEvent.click(save);
    await waitFor(() => expect(rec.of("config.save")).toHaveLength(1));
    const saved = rec.of("config.save")[0].config as Config;
    expect(saved.plugins).toEqual({ auto_update: "off", crash_threshold: 3 });
    expect(saved.general.language).toBe("en");
    expect(saved.bridge).toEqual({ port: 0 });
    expect(await screen.findByText(/Restart Ghostify/)).toBeInTheDocument();
  });

  it("discards edits and surfaces validation errors from the backend", async () => {
    backend({ "config.get": () => config, "config.save": () => fail("config", "configuration error: marketplace.refresh_interval_secs must be at least 60") });
    renderWithToasts(<Settings />);
    const input = await screen.findByLabelText(/refresh interval/);
    await userEvent.clear(input);
    await userEvent.type(input, "10");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText(/at least 60/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(screen.getByLabelText(/refresh interval/)).toHaveValue(21600);
  });
});

describe("App", () => {
  it("navigates between pages", async () => {
    backend({
      "app.info": () => info, "patch.status": () => [], "theme.list": () => [], "theme.mode": () => "system",
      "plugin.list": () => [], "config.get": () => config,
    });
    renderWithToasts(<App />);
    expect(await screen.findByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Themes" }));
    expect(await screen.findByRole("heading", { name: "Themes" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Plugins" }));
    expect(await screen.findByRole("heading", { name: "Plugins" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(await screen.findByRole("heading", { name: "Settings" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Settings" })).toHaveAttribute("aria-current", "page");
  });
});
