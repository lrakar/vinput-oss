# Contributing

Thanks for looking. Vinput is a small program and meant to stay one: a voice keyboard that types, searches and records, with everything else built on its local API ([docs/api.md](docs/api.md)).

## Before you start

- Open an issue first for anything bigger than a fix, so we can agree on the shape of it.
- [ARCHITECTURE.md](ARCHITECTURE.md) has the crate map, the process model and what has to stay compatible between versions. Read the part about the thread you touch: the main thread never waits.

## Building and checking

```bash
cargo build --release
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

Vinput targets Windows; the toolchain is pinned in `rust-toolchain.toml`.

## Pull requests

- One change per pull request, with tests where the behavior is testable.
- Keep the style of the code around yours: short comments that say why, not what.
- By contributing you agree that your work is released under the GPL-3.0 license of this repository.
