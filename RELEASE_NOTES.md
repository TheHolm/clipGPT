# Release notes

This file lists user-facing changes for each tagged release. See `AGENTS.md`
for the versioning and tagging convention.

## v0.2.0 - First real release: config.ini support, prebuilt packages, Wayland-only clipboard

This is the first tagged release. User-facing changes:

- `api_key`, `api_url`, `model`, `api_type` and the rewrite prompt now come
  from `config.ini` instead of being hardcoded - see `config.ini.example`.
  `api_key` and `model` are required.
- The default rewrite prompt changed to a pirate-speak rewrite (was a fixed
  Australian-English spelling/grammar check).
- Prebuilt packages are now published on tagged releases: Linux `.deb`
  (x86_64 and aarch64, built against both Debian Trixie and Ubuntu 26.04),
  Windows `.exe` (x86_64 and aarch64), and FreeBSD `.pkg` (x86_64).
- **On Linux and FreeBSD, clipboard access is now Wayland-only - X11 is no
  longer supported.** See README.md "Clipboard backend" for why.
- Licensed under AGPL-3.0-or-later (`LICENSE` added).

### Details

- Removed a hardcoded OpenAI API key that had been committed to source;
  config loading (`src/configfile.rs`) rewritten to actually read
  `config.ini` (OS-specific path, falling back to `./config.ini`) instead
  of being a stub.
- Replaced `actually_beep` (pinned to `cpal ^0.15.3`, no FreeBSD/BSD backend)
  with a direct `cpal 0.18` dependency and a new `src/beep.rs`.
- Replaced the `clipboard` crate (unmaintained since 2022, real `xcb`
  soundness advisories - RUSTSEC-2022-0056, RUSTSEC-2025-0051,
  RUSTSEC-2020-0097, RUSTSEC-2021-0019) with per-OS backends in
  `src/clipboard_backend.rs`: `wl-clipboard-rs` (Wayland only) on
  Linux/FreeBSD, `clipboard-win` directly on Windows, and the old
  `clipboard` crate kept only for macOS's Cocoa backend (not a CI target,
  untested; a documented, scoped `cargo-audit` ignore covers it in
  `.cargo/audit.toml`).
- Fixed real `cargo-audit` findings: `bytes`/`openssl` patch-level updates,
  `reqwest` 0.11 -> 0.12 (dropped a vulnerable `h2`), and `mio` bumped to
  1.2.4 to fix an unrelated FreeBSD build break (a removed `libc` constant).
- Added `AGENTS.md` (adapted from `TheHolm/dak`'s conventions) and a
  `.woodpecker/release.yaml` tag-triggered CI pipeline (Rust 1.92, Debian
  Trixie base) replacing the old GitHub Actions workflow, whose
  `aarch64-pc-windows-gnu` (not a real rustc target) and macOS
  (`cross-rs` has no legally redistributable Apple SDK image) targets never
  actually worked. Every target in the new pipeline was individually
  verified by actually building it.
- Added `debian/copyright` with the full AGPL-3.0-or-later text:
  `cargo-deb`'s auto-generated copyright only names the licence, not its
  text (Debian Policy 12.5).
- Added `config.ini.example`.
