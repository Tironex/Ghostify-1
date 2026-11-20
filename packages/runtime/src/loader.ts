import { bridgeUrls, readConfig, type GhostifyConfig } from "./config";
import { BridgeClient, RpcError, type ClientOptions } from "./rpc";
import { MediaSessionPlayer, ToastUi } from "./plugins/adapters";
import { PluginManager } from "./plugins/manager";
import { PlayerLink } from "./player-link";
import { isScheme, ThemeApplier } from "./theme";

export interface Loader {
  client: BridgeClient;
  theme: ThemeApplier;
  plugins: PluginManager;
  playerLink: PlayerLink;
  stop(): void;
}

/** Wires the bridge to the document: theme CSS, colour scheme, live reload. */
export function startLoader(
  config: GhostifyConfig,
  doc: Document,
  clientOptions: Partial<ClientOptions> = {},
  pluginOptions: Partial<ConstructorParameters<typeof PluginManager>[0]> = {},
): Loader {
  const client = new BridgeClient({ urls: bridgeUrls(config), secret: config.secret, ...clientOptions });
  const theme = new ThemeApplier(doc);

  client.on("theme.css", (params) => {
    const css = (params as { css?: unknown } | null)?.css;
    if (typeof css === "string") theme.applyCss(css);
  });
  client.on("theme.scheme", (params) => {
    const scheme = (params as { scheme?: unknown } | null)?.scheme;
    if (isScheme(scheme)) theme.applyScheme(scheme);
  });

  client.onState((state) => {
    if (state !== "open") return;
    void client
      .call("session.hello", { client: "ghostify-loader", version: config.version })
      .then(() => syncTheme(client, theme))
      .catch((error) => {
        // A dropped connection already triggers a reconnect; only report failures on a live one.
        if (client.connectionState === "open") console.warn("[ghostify] handshake failed", error);
      });
  });

  const player = new MediaSessionPlayer(doc);
  const ui = new ToastUi(doc);
  const playerLink = new PlayerLink(client, player, ui);
  playerLink.start();

  const plugins = new PluginManager({
    client,
    doc,
    adapters: { player, ui },
    version: config.version,
    ...pluginOptions,
  });
  plugins.start();

  client.start();
  return {
    client,
    theme,
    plugins,
    playerLink,
    stop: () => {
      playerLink.stop();
      plugins.stopAll();
      client.stop();
    },
  };
}

/** Fetches the active theme after (re)connecting. Older servers without the method are tolerated. */
async function syncTheme(client: BridgeClient, theme: ThemeApplier): Promise<void> {
  try {
    const state = await client.call<{ css?: unknown; scheme?: unknown } | null>("theme.current");
    if (state && typeof state.css === "string") theme.applyCss(state.css);
    if (state && isScheme(state.scheme)) theme.applyScheme(state.scheme);
  } catch (error) {
    if (error instanceof RpcError && error.code === -32601) return;
    throw error;
  }
}

export { readConfig };
