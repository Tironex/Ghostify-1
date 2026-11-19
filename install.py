#!/usr/bin/env python3
"""Builds Ghostify from source and, if asked, patches Spotify.

Usage:
    python install.py              build the desktop app and the command line tool
    python install.py --check      only check that everything needed is installed
    python install.py --cli-only   build only the command line tool (no WebView needed)
    python install.py --patch      after building, patch Spotify (close Spotify first)
    python install.py --status     show the Spotify installations Ghostify found

Needs Python 3.8+, Rust (rustup.rs), Node.js 20+ and pnpm. Nothing outside the project
folder is changed, except that `cargo install` may add `tauri-cli` to your Cargo bin folder
and pnpm may be installed through npm when it is missing.
"""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path
from typing import List, Optional, Sequence

ROOT = Path(__file__).resolve().parent
UI_DIR = ROOT / "packages" / "ui"
TAURI_DIR = ROOT / "src-tauri"
SYSTEM = platform.system()  # "Windows", "Linux" or "Darwin"
EXE = ".exe" if SYSTEM == "Windows" else ""

MIN_NODE = 20
MIN_RUST = (1, 85)


class InstallError(Exception):
    """A step failed; the message tells the user what to do."""


def say(message: str) -> None:
    print(f"\n==> {message}", flush=True)


def run(command: Sequence[str], cwd: Optional[Path] = None, capture: bool = False) -> str:
    """Runs a command, streaming its output unless `capture` is set."""
    shown = " ".join(command)
    print(f"$ {shown}", flush=True)
    try:
        result = subprocess.run(
            list(command),
            cwd=str(cwd or ROOT),
            check=False,
            text=True,
            stdout=subprocess.PIPE if capture else None,
            stderr=subprocess.STDOUT if capture else None,
        )
    except FileNotFoundError as error:
        raise InstallError(f"Could not start `{command[0]}`: {error}") from error
    if result.returncode != 0:
        tail = f"\n{result.stdout.strip()}" if capture and result.stdout else ""
        raise InstallError(f"`{shown}` failed with exit code {result.returncode}.{tail}")
    return result.stdout or ""


def tool(name: str) -> Optional[str]:
    """Full path of an executable on PATH (this also finds `pnpm.cmd` on Windows)."""
    return shutil.which(name)


def version_of(command: Sequence[str]) -> Optional[str]:
    try:
        result = subprocess.run(list(command), capture_output=True, text=True, check=False)
    except OSError:
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def parse_version(text: str) -> Optional[tuple]:
    digits = "".join(ch if (ch.isdigit() or ch == ".") else " " for ch in text).split()
    for chunk in digits:
        parts = chunk.split(".")
        if len(parts) >= 2 and all(p.isdigit() for p in parts[:2]):
            return tuple(int(p) for p in parts[:3] if p.isdigit())
    return None


def check_python() -> None:
    if sys.version_info < (3, 8):
        raise InstallError("Python 3.8 or newer is required.")


def check_rust() -> str:
    cargo = tool("cargo")
    if not cargo:
        raise InstallError("Rust is missing. Install it from https://rustup.rs and open a new terminal.")
    text = version_of(["rustc", "--version"]) or ""
    found = parse_version(text)
    if found is None or found[:2] < MIN_RUST:
        raise InstallError(
            f"Rust {MIN_RUST[0]}.{MIN_RUST[1]} or newer is required (found: {text or 'unknown'}). "
            "Run `rustup update stable`."
        )
    print(f"Rust: {text}")
    return cargo


def check_node() -> None:
    text = version_of(["node", "--version"]) or ""
    found = parse_version(text)
    if found is None:
        raise InstallError("Node.js is missing. Install version 20 or newer from https://nodejs.org.")
    if found[0] < MIN_NODE:
        raise InstallError(f"Node.js {MIN_NODE} or newer is required (found: {text}).")
    print(f"Node.js: {text}")


def ensure_pnpm() -> str:
    pnpm = tool("pnpm")
    if pnpm:
        print(f"pnpm: {version_of([pnpm, '--version'])}")
        return pnpm
    say("pnpm is missing, trying to install it")
    corepack = tool("corepack")
    if corepack:
        try:
            run([corepack, "enable", "pnpm"])
        except InstallError:
            pass
    pnpm = tool("pnpm")
    if not pnpm:
        npm = tool("npm")
        if not npm:
            raise InstallError("pnpm is missing and npm was not found. Install Node.js from https://nodejs.org.")
        run([npm, "install", "-g", "pnpm"])
        pnpm = tool("pnpm")
    if not pnpm:
        raise InstallError("pnpm was installed but is not on PATH yet. Open a new terminal and run this script again.")
    return pnpm


def check_linux_libraries() -> None:
    """The desktop app needs the WebKitGTK development files on Linux."""
    if SYSTEM != "Linux":
        return
    pkg_config = tool("pkg-config")
    needed = ["webkit2gtk-4.1", "gtk+-3.0", "librsvg-2.0"]
    if not pkg_config:
        raise InstallError(
            "pkg-config is missing. On Debian/Ubuntu run:\n"
            "  sudo apt install pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev build-essential"
        )
    missing = [name for name in needed if subprocess.run([pkg_config, "--exists", name]).returncode != 0]
    if missing:
        raise InstallError(
            f"Missing system libraries ({', '.join(missing)}). On Debian/Ubuntu run:\n"
            "  sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev build-essential\n"
            "Other distributions: install the equivalent WebKitGTK 4.1, GTK 3 and librsvg development packages."
        )
    print("System libraries: WebKitGTK, GTK, librsvg found")


def check_windows_tools() -> None:
    if SYSTEM != "Windows":
        return
    if not tool("link") and not tool("cl"):
        print(
            "Note: the MSVC linker was not found on PATH. If the build fails, install the "
            "\"Build Tools for Visual Studio\" with the C++ workload, or run this script from "
            "the \"x64 Native Tools Command Prompt\"."
        )


def ensure_tauri_cli(cargo: str) -> None:
    if version_of([cargo, "tauri", "--version"]):
        print(f"tauri-cli: {version_of([cargo, 'tauri', '--version'])}")
        return
    say("Installing tauri-cli (one time, takes a few minutes)")
    run([cargo, "install", "tauri-cli", "--version", "^2", "--locked"])


def build_cli(cargo: str) -> Path:
    say("Building the command line tool")
    run([cargo, "build", "--release", "-p", "ghostify-app", "--bin", "ghostify"])
    binary = ROOT / "target" / "release" / f"ghostify{EXE}"
    if not binary.is_file():
        raise InstallError(f"The build finished but {binary} does not exist.")
    return binary


def build_desktop(cargo: str, pnpm: str) -> List[Path]:
    say("Installing interface dependencies")
    run([pnpm, "install", "--frozen-lockfile"], cwd=UI_DIR)
    ensure_tauri_cli(cargo)
    say("Building the desktop app (this takes a while the first time)")
    run([cargo, "tauri", "build"], cwd=TAURI_DIR)
    bundle = TAURI_DIR / "target" / "release" / "bundle"
    suffixes = {".exe", ".msi", ".dmg", ".deb", ".rpm", ".appimage"}
    found = sorted(p for p in bundle.rglob("*") if p.is_file() and p.suffix.lower() in suffixes)
    if not found:
        found = [p for p in [TAURI_DIR / "target" / "release" / f"ghostify-desktop{EXE}"] if p.is_file()]
    return found


def spotify_warning() -> None:
    print(
        "\nClose Spotify completely before patching. Ghostify makes a backup first, and\n"
        "`ghostify restore` brings the original back. Changing the Spotify client may go against\n"
        "Spotify's terms of use; you do this at your own risk."
    )


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build Ghostify from source.")
    parser.add_argument("--check", action="store_true", help="only check the requirements")
    parser.add_argument("--cli-only", action="store_true", help="build only the command line tool")
    parser.add_argument("--patch", action="store_true", help="patch Spotify after building")
    parser.add_argument("--status", action="store_true", help="show the Spotify installations and exit")
    return parser.parse_args(list(argv))


def main(argv: Sequence[str]) -> int:
    args = parse_args(argv)
    os.chdir(ROOT)
    try:
        say("Checking requirements")
        check_python()
        cargo = check_rust()
        check_windows_tools()
        pnpm: Optional[str] = None
        if not args.cli_only:
            check_node()
            pnpm = ensure_pnpm()
            check_linux_libraries()
        if args.check:
            say("Everything needed is installed")
            return 0

        cli = build_cli(cargo)
        if args.status:
            run([str(cli), "status"])
            return 0

        installers: List[Path] = []
        if not args.cli_only:
            assert pnpm is not None
            installers = build_desktop(cargo, pnpm)

        say("Done")
        print(f"Command line tool: {cli}")
        if installers:
            print("Desktop app / installers:")
            for path in installers:
                print(f"  {path}")
        elif not args.cli_only:
            print(f"Desktop app: see {TAURI_DIR / 'target' / 'release'}")

        if args.patch:
            spotify_warning()
            answer = input("Patch Spotify now? [y/N] ").strip().lower()
            if answer in ("y", "yes", "j", "ja"):
                run([str(cli), "patch"])
                print("Spotify is patched. Start Spotify and keep Ghostify running (`ghostify run` or the app).")
            else:
                print(f"Skipped. Patch later with: {cli} patch")
        else:
            print(f"\nTo patch Spotify later: {cli} patch   (or use the Patch button in the app)")
        return 0
    except InstallError as error:
        print(f"\nError: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\nCancelled.", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
