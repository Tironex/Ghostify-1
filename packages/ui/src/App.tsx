import { useState, type ComponentType } from "react";
import { Dashboard } from "./pages/Dashboard";
import { Extras } from "./pages/Extras";
import { MiniPlayer } from "./pages/MiniPlayer";
import { Marketplace } from "./pages/Marketplace";
import { Plugins } from "./pages/Plugins";
import { Settings } from "./pages/Settings";
import { Themes } from "./pages/Themes";
import { Icon, Logo } from "./ui/brand";
import { NavContext } from "./ui/nav";
import { PlayerBar } from "./ui/PlayerBar";
import { ToastProvider } from "./ui/toast";

type IconName = Parameters<typeof Icon>[0]["name"];

const pages: { id: string; label: string; icon: IconName; Page: ComponentType }[] = [
  { id: "dashboard", label: "Dashboard", icon: "home", Page: Dashboard },
  { id: "themes", label: "Themes", icon: "palette", Page: Themes },
  { id: "plugins", label: "Plugins", icon: "plug", Page: Plugins },
  { id: "marketplace", label: "Marketplace", icon: "store", Page: Marketplace },
  { id: "extras", label: "Extras", icon: "spark", Page: Extras },
  { id: "settings", label: "Settings", icon: "gear", Page: Settings },
];

/** The mini player window shows only the player, without the navigation. */
export function isMiniPlayer(hash: string): boolean {
  return hash === "#/mini";
}

export function App() {
  const [current, setCurrent] = useState("dashboard");
  if (isMiniPlayer(window.location.hash)) {
    return (
      <ToastProvider>
        <MiniPlayer />
      </ToastProvider>
    );
  }
  const Page = pages.find((p) => p.id === current)?.Page ?? Dashboard;
  return (
    <ToastProvider>
      <NavContext.Provider value={setCurrent}>
        <div className="flex h-screen flex-col bg-zinc-950">
          <div className="flex min-h-0 flex-1 gap-2 p-2 pb-0">
            <nav aria-label="Main" className="flex w-56 shrink-0 flex-col rounded-2xl border border-white/10 bg-zinc-950 p-3">
              <div className="mb-6 px-2 pt-2">
                <Logo />
              </div>
              <ul className="space-y-1">
                {pages.map(({ id, label, icon }) => (
                  <li key={id}>
                    <button
                      type="button"
                      aria-current={current === id ? "page" : undefined}
                      onClick={() => setCurrent(id)}
                      className={`flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm font-medium transition-colors ${
                        current === id ? "bg-white/12 text-white" : "text-zinc-400 hover:bg-white/6 hover:text-white"
                      }`}
                    >
                      <Icon name={icon} />
                      {label}
                    </button>
                  </li>
                ))}
              </ul>
            </nav>
            <main className="min-w-0 flex-1 overflow-y-auto rounded-2xl border border-white/10 bg-[radial-gradient(ellipse_70%_35%_at_85%_0%,rgba(255,255,255,0.07),transparent_70%)] p-6">
              <Page key={current} />
            </main>
          </div>
          <PlayerBar />
        </div>
      </NavContext.Provider>
    </ToastProvider>
  );
}
