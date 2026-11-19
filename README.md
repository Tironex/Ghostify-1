# Ghostify

Themes, plugins and a marketplace for Spotify Desktop. Open source, no server of our own:
the marketplace is a signed file on GitHub, everything else runs on your computer.

## Layout

| Path | What it is |
| --- | --- |
| `crates/ghostify-core` | configuration, paths, events, errors |
| `crates/ghostify-db` | SQLite storage and migrations |
| `crates/ghostify-spotify` | finds Spotify installations and their versions |
| `crates/ghostify-patch` | patches `xpui.spa`, backups, repair, automatic repatch |
| `crates/ghostify-bridge` | local JSON-RPC bridge between the app and the loader in Spotify |
| `crates/ghostify-themes` | theme store, CSS handling, live reload |
| `crates/ghostify-plugins` | plugin store, scanner, permissions, crash handling |
| `crates/ghostify-market` | signed index, verified downloads, search, updates; `ghostify-registry` tool |
| `crates/ghostify-extras` | player state, lyrics, Discord presence, hotkeys |
| `crates/ghostify-app` | wires everything together; `ghostify` command line; `api` for the interface |
| `packages/runtime` | the loader that runs inside Spotify (TypeScript) |
| `packages/sdk` | typings and schema for plugin authors |
| `packages/ui` | the desktop interface (React, Tailwind) |
| `src-tauri` | Tauri shell around `ghostify-app` and `packages/ui` |

## Build and test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
(cd packages/runtime && pnpm install && pnpm test)
(cd packages/ui && pnpm install && pnpm test)
```

After changing `packages/runtime`, run `scripts/sync-runtime.sh`. It rebuilds the loader and copies it to
`crates/ghostify-app/assets/`, from where it is embedded into the application.

## Command line

`cargo run -p ghostify-app --bin ghostify -- status` (also `patch`, `repair`, `restore`, `theme …`, `plugin …`,
`market …`, and `run` for the background service).

## Desktop app

`src-tauri` is not part of the Cargo workspace because it needs the system WebView libraries
(Linux: `libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev`).

```sh
cd src-tauri
cargo install tauri-cli --version "^2" --locked
cargo tauri dev      # development
cargo tauri build    # installers
```

## Marketplace registry

The registry is a repository that publishes a signed `index.json` (GitHub Pages).

```sh
cargo run -p ghostify-market --bin ghostify-registry -- keygen registry.key       # prints the public key
cargo run -p ghostify-market --bin ghostify-registry -- sign registry.key main index.json signed-index.json
```

Build releases with `GHOSTIFY_MARKET_KEY=main:<public key>` so the application trusts that key. Keep `registry.key` secret.
The default index address is `https://ghostify.github.io/registry/index.json`; change `marketplace.index_url` or the default
in `crates/ghostify-core/src/config.rs` to your own registry.

## Extras

* **Mini player and media controls:** the loader reports the current track to the app; the mini player window (`#/mini`) shows it with lyrics and controls.
* **Hotkeys:** global shortcuts, editable under Extras; media keys such as `MediaPlayPause` work as shortcuts.
* **Lyrics:** `.lrc` or `.txt` files named `Artist - Title` in the lyrics folder. Optional lookup on lrclib.net (off by default).
* **Discord:** needs your own application id from discord.com/developers; set it under Extras.

## The Ghost look

`themes/ghostify-ghost` is the black and white theme Ghostify ships with: deep black
surfaces, thin light borders, white accents, grey cover art. It is installed on first start
and activated only if no theme is active yet; it is never forced back afterwards. It has two
colour schemes (`Ghost`, `Ash`) in `color.ini` and can be edited like any other theme, and
live reload applies changes while Spotify is open. The desktop app uses the same look.
Spotify's class names change between releases, so the stylesheet sticks to long-lived layout
hooks and sets Spotify's own colour variables as well; selectors that no longer match are
simply ignored. It has not been tried against a real Spotify build.

## Limits you should know

* Not tested against a real Spotify client; selectors used by the player adapter (`data-testid`) may need adjusting.
* The Microsoft Store and Snap versions of Spotify cannot be patched.
* The Spicetify compatibility layer covers a subset of the Spicetify API and is off by default.
* Not built: equalizer and audio visualizer (Spotify does not give the page access to the audio), custom home screen and
  AI playlist generator (they need Spotify's internal interfaces and, for the generator, an AI model that does not need a server).
* The Discord presence, the lrclib.net lookup and the selectors for playback position are written from the public documentation
  and are not verified against the live services.
