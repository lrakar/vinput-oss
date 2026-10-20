<div align="center">

<img src="assets/vinput-logo.png" width="120" alt="VinPUT logo">

# VinPUT

**Hold a key, talk, and it types into any app on Windows. Free, local, open source.**

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.6.2-blue)](https://putlab.net/vinput)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11-0078D4)](https://putlab.net/vinput)
[![GitHub stars](https://img.shields.io/github/stars/lrakar/vinput-oss?style=social)](https://github.com/lrakar/vinput-oss)

[**Download for Windows**](https://putlab.net/vinput) &nbsp;·&nbsp; Version 0.6.2, 3.3 MB

or with winget: `winget install PUTLAB.VinPUT`

<sub>The winget package is listed once it is accepted into the winget repository.</sub>

<br>

<img src="docs/images/settings.png" width="720" alt="VinPUT Settings window">

</div>

## Why VinPUT

- **Types into any app.** Your words arrive as keystrokes in whatever text field has focus.
- **Two keys, two jobs.** **F1** types what you said. **F2** types it and presses Enter, for chat boxes and prompts.
- **Local speech models.** Six models run on this PC, on the processor or on a graphics card of any make, and nothing you say leaves the PC.
- **Cloud models when you want them.** Google Cloud Speech and Gemini work with your own key or project. VinPUT Pro can hand over to a model on this PC mid-dictation if its server cannot be reached, without losing a word.
- **Small.** The installer is 3.3 MB. Speech models are downloaded separately, with a checksum for each file.
- **Extensible.** Add your own keys that run a PowerShell command, a script or any program with what you said.

## Compared

| | VinPUT | Windows Voice Typing (Win+H) | Wispr Flow | Superwhisper |
| --- | --- | --- | --- | --- |
| Price model | Free; optional paid plans | Free, built into Windows | Free plan, paid subscription | Free plan, paid subscription |
| Open source | Yes, GPL-3.0 | No | No | No |
| Works offline | Yes, with local models | Not stated | Not stated | Yes |
| Runs on Windows | Yes (10 and 11) | Yes | Yes | Yes |

<sub>Competitor rows are summarized from the comparison on putlab.net/vinput and from public product information. Prices are left out on purpose; check the current ones before you decide.</sub>

## Quick start

1. **Install.** Download `VinputSetup-0.6.2.exe` from [putlab.net/vinput](https://putlab.net/vinput) or use `winget install PUTLAB.VinPUT`, run it and press **Install**. No administrator rights are needed. VinPUT starts and lives in the system tray.
2. **Pick a speech model.** The first start opens Settings on the Models page. Choose a model and press **Download**. Local models need no account.
3. **Dictate.** Click into any text field, tap **F1**, speak, and tap again to stop. Or hold **F1**, speak and let go. Your words are typed where the cursor is.

## Keys

Keys are set under Settings, Shortcuts. F1 and F2 are the defaults.

| Key | What it does |
| --- | --- |
| **F1** | Dictates and types what you said |
| **F2** | Dictates, types it, then presses Enter |
| **Escape** | Cancels a dictation, search or memo that is running; nothing more is typed, opened or saved |
| **Enter** | While F1 or F2 is listening, stops it, types the last words and presses Enter once |
| Dictated search key (yours) | Speak what you are looking for; the search opens in your default browser |
| Voice memo key (yours) | Records an MP3 to `Documents\Vinput memos` and copies it to the clipboard, ready for Ctrl+V |
| Edit by voice key (yours) | Select text, hold the key and say the change, such as "make it shorter" |
| Your action key (yours) | Runs a program or script you choose with what you said |

Settings, Cleanup can tidy a dictation before it is typed: **Clean** removes filler words and repeats and fixes punctuation, **Coding prompt** and **Email** rewrite it for that use, and **As said** types it unchanged.

## Speech models

| Model | Runs | Text arrives | Size or needs |
| --- | --- | --- | --- |
| Google Cloud Speech (Chirp 3) | Online | As you speak | Needs a Google Cloud project |
| Google Gemini 3.5 Transcribe | Online | As you speak | Needs a Gemini API key |
| Qwen3-ASR 1.7B | On this PC | When you stop | 2.6 GB |
| Qwen3-ASR 0.6B | On this PC | When you stop | 1.1 GB |
| NVIDIA Parakeet TDT v3 | On this PC | When you stop | 677 MB |
| Whisper large-v3 turbo | On this PC | When you stop | 1.6 GB |
| Whisper large-v3 turbo (compact) | On this PC | When you stop | 582 MB |
| Whisper base | On this PC | When you stop | 156 MB |

Local models are downloaded from pinned release addresses and checked against a pinned SHA-256; an interrupted download continues where it stopped. The Models page shows whether each model will run well on your PC. Graphics card use is switched on by default when a card is found. You can also add your own Whisper model from Hugging Face or a file on disk.

VinPUT Plus and Pro are online recognition by subscription, with a sign-in and no keys to set up. When their monthly hours run out, dictation carries on with a model on this PC.

## Build from source

Requires the Rust toolchain pinned in `rust-toolchain.toml`.

```bash
cargo build --release
```

```bash
target\release\vinput.exe
```

A release build starts with Windows sign-in (switch it off in the tray menu). Debug builds never do. If VinPUT is installed and running, use the one-shot commands below rather than starting a second copy without arguments.

A few one-shot commands from the repository:

```bash
cargo run -- transcribe clip.wav
```

```bash
cargo run -- type "Hello from VinPUT"
```

```bash
cargo run -- models
```

Developer copies keep their settings and models apart from the installed app. The crate map, the process model and the rest of the commands are in [ARCHITECTURE.md](ARCHITECTURE.md).

## Extending VinPUT

VinPUT does dictation, search and voice memos. Everything else is built on its local API, a named pipe at `\\.\pipe\vinput`, which lets other programs start and stop a session, receive a dictation's text and learn where memos are saved. See [docs/api.md](docs/api.md).

## Limits

- Windows blocks typing into windows of administrator (elevated) apps.
- Releases are not code-signed yet, so Windows SmartScreen may warn on first run.
- While VinPUT runs, it takes its keys exclusively, so the focused app does not see them.
- Google Cloud text arrives a sentence at a time. Local models deliver the whole dictation when you stop.
- In apps that do not expose their text cursor, the listening indicator appears at the bottom of their window.

## Contributing

Open an issue before anything bigger than a fix, and read [CONTRIBUTING.md](CONTRIBUTING.md) and [ARCHITECTURE.md](ARCHITECTURE.md) first. Tests and checks:

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

## License

VinPUT is free software under the [GNU General Public License, version 3](LICENSE).

---

If VinPUT saves you typing, a star helps others find it.
