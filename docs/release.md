# Releasing Vinput

A release is three files on the Vinput site:

- `releases/VinputSetup-<version>.exe`: the setup program. It is the Vinput executable under a name that starts with `VinputSetup`, which is what makes it install instead of run.
- `releases/latest.json`: the manifest every installed copy reads to learn about the newest release.
- `releases/history.json`: every release with its notes, which the site's release notes page shows.

What is new is written once, in `releases.json` at the root of this repository. The update window of the app and the release notes page of the site both come from it.

```json
{
  "version": "0.3.0",
  "notes": ["Voice memos", "Local models"],
  "url": "https://vinput-dasvcr.web.app/releases/VinputSetup-0.3.0.exe",
  "sha256": "…64 hexadecimal digits…",
  "bytes": 6127616,
  "published": "2026-10-05"
}
```

## Steps

1. Set the new version in the root `Cargo.toml`, under `[workspace.package]`; every crate takes its version from there. Run `cargo check` so `Cargo.lock` follows (the release build is `--locked` and refuses a stale one), add an entry for the version at the top of `releases.json`, and commit all three. The notes are the lines the update window shows; keep them to five short ones.
2. Build the release and its manifest:

   ```bash
   powershell -ExecutionPolicy Bypass -File scripts\release.ps1
   ```

   It builds the `vinput` package with the `release` profile into `target\dist`, refuses an executable that would need the Visual C++ runtime, and writes the setup program and both manifests to `dist\releases`. It publishes nothing.
3. Try it before anyone else gets it. Serve `dist` on this PC and point one installed copy at it:

   ```bash
   python -m http.server 18765 --bind 127.0.0.1 --directory dist
   ```

   Build the manifest for that address with `-BaseUrl http://127.0.0.1:18765/releases`, quit the installed Vinput, and start it again from a shell in which the environment variable `VINPUT_UPDATE_URL=http://127.0.0.1:18765/releases/latest.json` is set (`& "$env:LOCALAPPDATA\Programs\Vinput\vinput.exe"`); then pick **Check for updates** in the tray menu. It has to be the installed copy: a build started through `cargo run`, or with `VINPUT_INSTANCE` set, is a separate copy, and a separate copy does not look for updates by itself and never installs one.
4. Publish: copy `dist\releases\VinputSetup-<version>.exe`, `history.json` and `latest.json` (built with the real address) into `site/releases` on the `site` branch, point the download links of the site at the new setup program, and deploy the site. Upload the setup program first and the manifest last, so the manifest never points at a file that is not there yet.

From the moment `latest.json` is live, running copies offer the update within six hours, or at once from **Check for updates**.

## How an update is trusted

The app only accepts a manifest and a download over HTTPS, and it runs the downloaded setup program only if its size and SHA-256 match the manifest. The manifest is as trustworthy as the site it comes from; anyone who can deploy the site can ship an update. Releases are not code-signed yet, so Windows SmartScreen warns when a freshly downloaded setup program is started by hand. Updates started by the app itself are not affected.

## What an update keeps

Everything outside the program folder: `%APPDATA%\Vinput` (settings, log) and `%LOCALAPPDATA%\Vinput` (models, engine), and the keys in the Windows credential store. A newer version reads an older settings file as it is, fills in defaults for settings it adds, and writes back any key it does not know. Never rename or repurpose a settings key; add a new one instead.

## How big a release may be

The setup program is the executable every user downloads and the one that stays in memory, so its size is watched. CI builds it on every push (the `size` job in `.github/workflows/ci.yml`), writes the size into the job summary and fails when it is over the budget set there. Locally:

```bash
cargo size
```

builds the same executable into `target\size\release\vinput.exe`. When a change needs more room, raise the budget in the same commit and say in its message what the bytes buy. What decides the size is in the root `Cargo.toml` (the `release` profile), in `.cargo/config.toml` (the C runtime is linked in) and in what `crates/vinput-core/Cargo.toml` depends on: one cryptography library, no gRPC server, and no HTTP client or sign-in library, because `net.rs` uses the one in Windows and `stt/google/token.rs` asks for tokens itself. Two more things keep it down. What a window is sent (`vinput-view`) goes over the line in serde's plain form: an enum with `tag` or `content` is read through a buffer, which compiles everything under it a second time, and cost some 190 kB when it was tried. And what is only for working on the windows (the canned scenes, the test card, taking pictures, `--mock` and `--picture`) is compiled into a build for development and the tests and left out of the one that ships.
