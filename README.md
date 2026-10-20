<div align="center">

<img src="assets/vinput-logo.png" width="120" alt="VinPUT logo">

# VinPUT

**Hold a key, talk, and it types into any app on Windows.**<br>
Free, open source, and it can run entirely on your PC.

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue)](LICENSE)
[![Latest release](https://img.shields.io/badge/version-0.6.2-blue)](https://github.com/lrakar/vinput-oss/releases/latest)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11-0078D4)](https://putlab.net/vinput)
[![GitHub stars](https://img.shields.io/github/stars/lrakar/vinput-oss?style=social)](https://github.com/lrakar/vinput-oss)

[**Download for Windows**](https://putlab.net/vinput) (0.6.2, 3.3 MB) &nbsp;·&nbsp; [Latest release](https://github.com/lrakar/vinput-oss/releases/latest)

or with winget: `winget install PUTLAB.VinPUT`

<sub>The winget package becomes available once it is accepted into the winget repository.</sub>

<br>

<img src="docs/images/settings.png" width="720" alt="VinPUT Settings window">

</div>

## What it does

VinPUT sits in the system tray. Click into any text field, press a key and speak. Your words are typed where the cursor is, as keystrokes, so it works in browsers, editors, chat apps and terminals alike.

- **Two keys.** **F1** types what you said. **F2** types it and presses Enter, for chat boxes and prompts.
- **Local speech models.** Six models run on this PC, on the processor or on a graphics card of any make. With one of them, your voice never leaves the machine.
- **Online models when you want them.** Google Cloud Speech and Gemini work with your own project or key.
- **Small.** The installer is 3.3 MB. Models are downloaded separately, and each file is checked against a pinned SHA-256.
- **Yours to extend.** Add a key that hands what you said to a PowerShell command, a script or any program.

Of the voice typing tools compared on [putlab.net/vinput](https://putlab.net/vinput) (Wispr Flow, Willow, Aqua Voice, Superwhisper), VinPUT is the only open source one. It is also Windows only.

## Quick start

1. **Install.** Download `VinputSetup-0.6.2.exe` from [putlab.net/vinput](https://putlab.net/vinput), run it and press **Install**. No administrator rights are needed. VinPUT starts and stays in the system tray.
2. **Pick a speech model.** The first start opens Settings on the Models page. Choose a model and press **Download**. Local models need no account.
3. **Dictate.** Click into a text field, tap **F1**, speak, and tap again to stop. Or hold **F1**, speak and let go.

## Keys

Keys are set under Settings, Shortcuts. F1 and F2 are the defaults; the others have no key until you set one.

| Key | What it does |
| --- | --- |
| **F1** | Dictates and types what you said |
| **F2** | Dictates, types it, then presses Enter |
| **Escape** | Cancels a running dictation, search or memo: nothing more is typed, opened or saved |
| **Enter** | While F1 or F2 is listening, stops it, types the last words and presses Enter once |
| Search key | Speak what you are looking for; the search opens in your default browser |
| Voice memo key | Records an MP3 to `Documents\Vinput memos` and puts it on the clipboard, ready to paste |
| Edit by voice key | Select text, hold the key and say the change, such as "make it shorter" |
| Action key | Runs a program or script you choose, with what you said |

Settings, Cleanup can tidy a dictation before it is typed. **Clean** removes filler words and repeats and fixes punctuation, **Coding prompt** and **Email** rewrite it for that use, and **As said** types it unchanged. Cleanup and Edit by voice use a text model on this PC or an online plan.

## Speech models

| Model | Runs | Download |
| --- | --- | --- |
| Google Cloud Speech (Chirp 3) | Online, needs a Google Cloud project | none |
| Google Gemini 3.5 Transcribe | Online, needs a Gemini API key | none |
| Qwen3-ASR 1.7B | On this PC, 30 languages | 2.5 GB |
| Qwen3-ASR 0.6B | On this PC, 30 languages | 1.0 GB |
| NVIDIA Parakeet TDT v3 | On this PC, 25 European languages | 669 MB |
| Whisper large-v3 turbo | On this PC, 99 languages | 1.6 GB |
| Whisper large-v3 turbo (compact) | On this PC, 99 languages | 574 MB |
| Whisper base | On this PC, 99 languages | 148 MB |

Each local model also needs a small speech engine (8 to 33 MB), downloaded once. Downloads come from pinned addresses, are checked against a pinned SHA-256, and an interrupted download continues where it stopped. The Models page tells you whether each model will run well on your PC, and a graphics card is used by default when one is found. You can also add your own Whisper model from Hugging Face or from a file.

VinPUT Plus and Pro are optional online plans with a sign-in and no keys to set up ([plans and prices](https://putlab.net/vinput#plans)). When a plan's monthly hours run out, or its server cannot be reached, dictation carries on with a model on this PC.

## Build from source

You need the Rust toolchain pinned in `rust-toolchain.toml`.

```bash
cargo build --release
target\release\vinput.exe
```

A release build starts with Windows sign-in; switch that off in the tray menu. Debug builds never do. If VinPUT is installed and running, use the one-shot commands below rather than starting a second copy without arguments.

```bash
cargo run -- transcribe clip.wav     # recognize a 16 kHz mono WAV
cargo run -- type "Hello from VinPUT"  # type into the focused window
cargo run -- models                  # list the speech models and their state
```

A developer copy keeps its settings and models apart from the installed app. The crate map, the process model and the other commands are in [ARCHITECTURE.md](ARCHITECTURE.md).

## Extending VinPUT

VinPUT does dictation, search and voice memos, and leaves the rest to other programs. They connect through a local API, a named pipe at `\\.\pipe\vinput`, to start and stop a session, receive a dictation's text and learn where memos are saved. See [docs/api.md](docs/api.md).

## Limits

- Windows does not allow typing into windows of administrator (elevated) apps.
- Releases are not code-signed yet, so Windows SmartScreen may warn on first run.
- While VinPUT runs, it takes its keys exclusively, so the focused app does not see them.
- Whisper models type at each pause. Qwen3-ASR and Parakeet type the whole dictation when you stop.
- In apps that do not expose their text cursor, the listening indicator appears at the bottom of the window.

## Contributing

Open an issue before anything bigger than a fix, and read [CONTRIBUTING.md](CONTRIBUTING.md) and [ARCHITECTURE.md](ARCHITECTURE.md) first. Before sending a change, run:

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

## License

VinPUT is free software under the [GNU General Public License, version 3](LICENSE).
