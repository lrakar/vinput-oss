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
  "published": "2026-10-05",
  "signature": "…the publisher's signature, in base64…"
}
```

## Steps

1. Set the new version in the root `Cargo.toml`, under `[workspace.package]`; every crate takes its version from there. Run `cargo check` so `Cargo.lock` follows (the release build is `--locked` and refuses a stale one), add an entry for the version at the top of `releases.json`, and commit all three. The notes are the lines the update window shows; keep them to five short ones.
2. Build the release and its manifest:

   ```bash
   powershell -ExecutionPolicy Bypass -File scripts\release.ps1
   ```

   It builds the `vinput` package with the `release` profile into `target\dist`, refuses an executable that would need the Visual C++ runtime, signs the release with the publisher's key (below), and writes the setup program and both manifests to `dist\releases`. It publishes nothing, and it stops before building anything if the key is not where it looks for it.
3. Try it before anyone else gets it. Serve `dist` on this PC and point one installed copy at it:

   ```bash
   python -m http.server 18765 --bind 127.0.0.1 --directory dist
   ```

   Build the manifest for that address with `-BaseUrl http://127.0.0.1:18765/releases`, quit the installed Vinput, and start it again from a shell in which the environment variable `VINPUT_UPDATE_URL=http://127.0.0.1:18765/releases/latest.json` is set (`& "$env:LOCALAPPDATA\Programs\Vinput\vinput.exe"`); then pick **Check for updates** in the tray menu. It has to be the installed copy: a build started through `cargo run`, or with `VINPUT_INSTANCE` set, is a separate copy, and a separate copy does not look for updates by itself and never installs one.

   Only a build for development listens to `VINPUT_UPDATE_URL` and takes a release from this PC; a build that ships, from the first signed release on, asks the site and nothing else. So the installed copy this is tried with has to be a build for development of the version before (`cargo build -p vinput`, then `target\debug\vinput.exe --install --quiet`), or a released version from before signatures, which still listens. Either way the manifest has to carry the real signature: a build for development checks it as a shipped one does.
4. Publish: copy `dist\releases\VinputSetup-<version>.exe`, `history.json` and `latest.json` (built with the real address) into `site/releases` on the `site` branch, point the download links of the site at the new setup program, and deploy the site. Upload the setup program first and the manifest last, so the manifest never points at a file that is not there yet.

From the moment `latest.json` is live, running copies offer the update within six hours, or at once from **Check for updates**.

## Releasing the hub and Voutput

The programs under `apps/` have a version of their own, in their own `Cargo.toml`, and are released apart from Vinput. A release of one is two files in its folder on the site: `hub/PUTLABSetup-<version>.exe` and `hub/latest.json` for the hub, `voutput/VoutputSetup-<version>.exe` and `voutput/latest.json` for Voutput.

```bash
powershell -ExecutionPolicy Bypass -Command "scripts\release-app.ps1 -Package voutput -Notes 'First line','Second line'"
```

`-Package` is `voutput` or `putlab-hub`. It builds and signs as `scripts\release.ps1` does, each under the name of its own setup program, and writes both files to `dist\voutput` or `dist\hub`. It publishes nothing. The notes are given on the command line; `releases.json` is Vinput's.

Publish as for Vinput: copy the two files into `site/voutput` or `site/hub` on the `site` branch and deploy, the setup program first and the manifest last.

Which programs the hub offers is `site/products.json` (`apps/hub/src/catalog.rs` has its form). A program is installable from the hub once it has an entry there without `"soon": true` and its manifest is live.

## How an update is trusted

The app asks the Vinput site for the manifest, downloads the setup program from that site and no other, and runs it only if its size and SHA-256 match the manifest. The checksum alone would make the manifest as trustworthy as the site: anyone who could deploy the site could ship an update. So the manifest is signed.

- **The key.** One Ed25519 key, the publisher's. The half that signs is a file that exists outside the repository and off the site, by default `%USERPROFILE%\.vinput-cloud\update-signing.pk8` (`PUTLAB_SIGNING_KEY` or `-SigningKey` name another). The half that checks is `PUBLISHER` in `crates/putlab-core/src/update.rs`, in every program.
- **What is signed** is four lines: what the name of the setup program begins with (`VinputSetup`), the version, the checksum, the size. So a signature is good for one file of one program and nothing else. The address is not signed; the checksum ties the file down wherever on the site it is.
- **What is checked.** A release whose signature does not hold is not offered, and not downloaded whoever asks. The hub checks the releases of the programs it installs the same way, each under its own name.
- **Keep the key, and keep it safe.** Whoever has the file can publish an update to every installed copy. And if it is lost no update can ever be signed again: installed copies would have to be replaced by hand with a build that carries a new `PUBLISHER`. Back it up somewhere that is not this PC. Nothing prints it; `cargo run -p putlab-core --example sign -- public <file>` prints the half that may be shown.
- **Versions from before signatures** (0.4.8 and earlier) do not look at the `signature` of a manifest and update by the checksum as they always did; that is how the first signed release reaches them. An old signed manifest put back on the site is still believed, and still not offered, because only a version newer than the running one is.

Releases are not code-signed for Windows yet, so SmartScreen warns when a freshly downloaded setup program is started by hand. Updates started by the app itself are not affected.

## What an update keeps

Everything outside the program folder: `%APPDATA%\Vinput` (settings, log) and `%LOCALAPPDATA%\Vinput` (models, engine), and the keys in the Windows credential store. A newer version reads an older settings file as it is, fills in defaults for settings it adds, and writes back any key it does not know. Never rename or repurpose a settings key; add a new one instead.

## How big a release may be

The setup program is the executable every user downloads and the one that stays in memory, so its size is watched. CI builds it on every push (the `size` job in `.github/workflows/ci.yml`), writes the size into the job summary and fails when it is over the budget set there. Locally:

```bash
cargo size
```

builds the same executable into `target\size\release\vinput.exe`. The programs under `apps/` have a budget each in the same job (`cargo build --profile small -p putlab-hub`, `cargo build --profile small -p voutput`): the hub is to stay under one megabyte, because it is only there to install the others, and Voutput well under Vinput, because its voices and models are downloads. Both are built with the profile `small` in the root `Cargo.toml`, which is the release profile under a name of its own. When a change needs more room, raise the budget in the same commit and say in its message what the bytes buy. What decides the size is in the root `Cargo.toml` (the `release` profile), in `.cargo/config.toml` (the C runtime is linked in) and in what `crates/vinput-core/Cargo.toml` depends on: one cryptography library, no gRPC server, and no HTTP client or sign-in library, because `net.rs` uses the one in Windows and `stt/google/token.rs` asks for tokens itself. Two more things keep it down. What a window is sent (`vinput-view`) goes over the line in serde's plain form: an enum with `tag` or `content` is read through a buffer, which compiles everything under it a second time, and cost some 190 kB when it was tried. And what is only for working on the windows (the canned scenes, the test card, taking pictures, `--mock` and `--picture`) is compiled into a build for development and the tests and left out of the one that ships.
