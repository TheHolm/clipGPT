# AGENTS.md

## Repository visibility

**This repository is pushed to a public Gitea instance (and mirrored to
GitHub for releases).** Before committing, pushing, or writing anything into
a tracked file (code, docs, scripts, test fixtures, commit messages), make
sure it contains no sensitive or identifying information: no real
hostnames/IPs, credentials/passwords/API keys/private keys, personal file
paths, or other details tied to a particular person's or machine's identity.
Genuinely throwaway material (ad hoc test scripts/local experiments written
for a single session) belongs outside the repo entirely (e.g. under `/tmp`),
never committed.

## Project overview

**clipGPT** is a small command-line tool that grabs whatever text is on the
clipboard, sends it to an OpenAI-compatible LLM API (OpenAI itself, or a
local server such as Ollama), and pastes the response back onto the
clipboard. It's meant to be bound to an OS-level hotkey or macro key rather
than run interactively. A beep plays when the response is ready or when an
error occurs.

## Stack

Rust, single binary crate. Configuration is `config.toml` with named
profiles, parsed by the `toml` crate into a plain `toml::Table`; profile
resolution (`include`/`clear`/`input`), strict validation and routing of
clipboard kinds to profiles are hand-written in `src/configfile.rs` rather
than a config framework - keep that in mind before reaching for something
heavier.

## Target platforms

Linux, Windows, and FreeBSD are first-class, hard-required CI targets; none
is best-effort. A change that would silently drop one of them (e.g. picking
a dependency with no FreeBSD backend) needs to be caught before merging, not
discovered later in CI. Clipboard access on Linux/FreeBSD is Wayland first
(via `wl-clipboard-rs`, needs the data-control protocol), with an X11
fallback (via pure-Rust `x11rb`) for GNOME/Mutter, which lacks that protocol,
and for pure X11 sessions - see README.md "Clipboard backend". Never bring
back the `xcb` crate (unmaintained, unsound). Audio is directly on `cpal` - verify both the
clipboard and audio backends still support a target before bumping either
dependency.

macOS is not built by CI: cross-compiling for it needs Apple's SDK, which
Apple's license doesn't allow redistributing, so there's no CI setup that
can produce one without someone supplying their own SDK. `cpal` and the
macOS-specific `objc2-app-kit` NSPasteboard bindings (see README.md
"Clipboard backend") do have genuine upstream macOS support though, so
building natively on a real Mac (no cross-compiling involved) will likely
work too - just untested, since no Mac hardware is available to this
project's CI. `cargo check --target aarch64-apple-darwin` needs no SDK and
should pass before merging anything touching the macOS backend.

## Layout

- `src/` - the binary (`main.rs`), config loading (`configfile.rs`), beep
  tone generation (`beep.rs`), per-OS clipboard access and password-marker
  detection (`clipboard_backend.rs`, with the X11 fallback and its
  background clipboard-owner helper in `clipboard_backend/x11.rs`), HTTP transport checks (`net.rs`),
  clipboard image checks and conversion to PNG (`imagefile.rs`),
  input/output cleaning (`sanitize.rs`), secret-pattern detection
  (`secrets.rs`) and command-line parsing (`cli.rs`)
- `config.toml.example` (short, one profile), `config.expanded.toml.example`
  (several profiles with includes) and `config.full.toml.example` (every
  key, with its default) - config templates users copy and fill in. The
  man page is the full reference; the examples only point to it (and to
  the README on GitHub for Windows, which gets no man page). Tests check
  all three parse, and keep the short one a single plain `[default]`
- `man/clip_gpt.1` - the man page (hand-written mdoc; lint with
  `mandoc -T lint -W warning`). Installed gzipped by both package types:
  `.deb` to `/usr/share/man/man1` (cargo-deb compresses it), FreeBSD `.pkg`
  to `/usr/local/share/man/man1` (`scripts/ci-freebsd.sh`). Tests in
  `cli.rs` and `configfile.rs` fail if it misses an option, a config key
  (`KEYS` in `configfile.rs`, which must also all appear in
  `config.full.toml.example`), or a changed default - update it with them
- `docker/` - a Dockerfile/compose setup for building in a clean container
  (build-only; clipboard/audio access isn't available from inside it)
- `scripts/` - CI tooling: FreeBSD packaging (`fetch-freebsd-deps.py`,
  `build-freebsd-pkg.py`, `ci-freebsd.sh`), download verification
  (`verify-freebsd-dist.sh`, `fetch-verified.sh`), a release-tag format
  guard (`check-release-tag.sh`), and release-notes extraction
  (`extract-release-notes.sh`). The Linux `.deb` uses `cargo-deb` directly
  instead (see `[package.metadata.deb]` in `Cargo.toml`); it installs to
  `/usr/bin` and `/usr/share`, never `/usr/local` (Debian Policy 9.1.2,
  same on Ubuntu), while the FreeBSD `.pkg` uses `/usr/local` as FreeBSD
  ports do
- `debian/copyright` - the full AGPL-3.0-or-later text, installed into every
  `.deb` explicitly (Debian Policy 12.5 - `cargo-deb`'s own auto-generated
  copyright only names the licence, not its text)
- `.woodpecker/release.yaml` - tag-triggered CI: builds Linux `.deb`,
  Windows binaries, and FreeBSD `.pkg` artifacts and publishes a GitHub
  release
- `RELEASE_NOTES.md` - detailed, user-facing history of tagged releases
- `AGENTS.md` - this file
- `LICENSE` - GNU Affero GPL v3 or later (`AGPL-3.0-or-later`)

## Build

```
cargo build --release
cargo test
cargo fmt
cargo clippy
```

Cross-compiling for FreeBSD needs no FreeBSD host - see `scripts/` and the
comments in `.woodpecker/release.yaml` for the sysroot/dependency-closure
approach (no real `pkg` binary involved, package built by hand).

## Releases & packaging

A tag `vX.Y.Z` matching the version in `Cargo.toml` triggers
`.woodpecker/release.yaml`, which builds all three CI target platforms and
publishes them to a GitHub release. Tags are always created manually, never
by CI itself. Publishing to GitHub requires this repo to be mirrored there
from Gitea (tags synced promptly) and a `github_token` secret configured in
Woodpecker (Project Settings -> Secrets, scoped to `Contents: read/write`,
restricted to the `tag` event) - neither of those is set up by anything in
this repo.

Any change that can affect a built package (source, build files, packaging
scripts, `Cargo.toml`/`Cargo.lock`) must be made on a branch, never committed
directly to the default branch. The default branch accepts direct commits
only for changes that cannot affect the package: documentation, tests-only
changes, or CI/tooling changes that don't alter packaged output.

- When merging a branch that will **not** be tagged as a release, summarise
  all changes in the code since the branch started (or the last merge) and
  use that summary as the merge description.
- When merging a branch that **will** be tagged as a release, the merge
  commit description contains only user-affecting changes (new features,
  bug fixes, changed behavior) - no low-level implementation detail. Add a
  new entry to `RELEASE_NOTES.md` with that same user-facing summary plus
  all the low-level detail that would otherwise have gone in the merge
  commit description.
- When starting work on each new branch, ask the user whether to bump the
  version number (and if so, to what value) before writing any code.
- Version numbers follow `X.Y.Z`: `X` (major) is bumped only when the user
  explicitly asks for it; `Y` (minor) is bumped when a change adds a new
  feature; `Z` (patch) is bumped for bugfixes and other changes that don't
  add, remove, or change functionality.

## Conventions

- `cargo fmt` style, lint with `cargo clippy`
- Every function is documented with a `///` doc comment describing its
  purpose and any non-obvious behavior
- All new code must be covered by tests; never add production code without
  accompanying tests (`cargo test`; unit tests live in each module)
- Commits include a detailed description of what changed and why
- **Never commit changes unless the user explicitly asks to commit**
- **Never create or push a git tag unless the user explicitly asks** - a tag
  triggers a real release build and publish
