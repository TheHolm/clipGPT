# Release notes

This file lists user-facing changes for each tagged release. See `AGENTS.md`
for the versioning and tagging convention.

## v0.3.1 - Clipboard works on GNOME (and X11) again

User-facing changes:

- **Fixed: clipGPT failed on GNOME** (e.g. Debian trixie, Ubuntu) with
  "A required Wayland protocol (ext-data-control, or wlr-data-control
  version 1) is not supported by the compositor". GNOME doesn't offer that
  protocol, so clipGPT now falls back to the X11 clipboard (through
  Xwayland), which GNOME keeps in sync with the Wayland one. Compositors
  that do support the protocol (KDE Plasma, Sway, Hyprland, ...) keep
  using Wayland directly.
- **Pure X11 sessions are supported again** through the same fallback.
- Password-manager detection (`x-kde-passwordManagerHint`) works on the
  X11 path too.
- On X11, a small background `clip_gpt --x11-clipboard-owner` process
  stays running after each run to keep the response on the clipboard. It
  exits as soon as something else is copied.
- If no clipboard is reachable at all, the error message now gives the
  reason for both Wayland and X11.

### Details

- New dependency `x11rb` 0.14 (Linux/FreeBSD only), using its pure-Rust
  `RustConnection`: no libxcb, no new native library, so neither the
  `.deb` dependencies nor the FreeBSD sysroot/dependency closure change.
  It does not pull in the `xcb` crate (still only reachable from the
  macOS-only `clipboard` crate); `cargo audit` reports exactly the same
  findings as v0.3.0.
- `src/clipboard_backend.rs`: Wayland is still tried first. The fallback
  runs only for errors meaning Wayland clipboard access is unavailable
  (`MissingProtocol`, `WaylandConnection`, `SocketOpenError`), and only if
  `DISPLAY` is non-empty. Other Wayland errors (empty clipboard, no seats,
  ...) behave as before. `wayland_marks_password` was renamed
  `marks_password`, since X11 targets use the same hint name.
- New `src/clipboard_backend/x11.rs`:
  - Reading: checks the owner, converts `TARGETS` (at most 256 entries)
    to detect the password hint and pick a text format (`UTF8_STRING`,
    `text/plain;charset=utf-8`, `STRING` (decoded as Latin-1), `TEXT`,
    `text/plain`), then converts the text. `INCR` transfers are supported.
    The `max_input_bytes` limit is enforced while receiving: the
    `GetProperty` length is capped at `max_bytes + 1`, the `INCR` size
    announcement and each chunk are checked, and nothing over the limit
    is returned in part. Every wait for the owner or server has a 3 s
    timeout. If the owner doesn't answer `TARGETS`, `UTF8_STRING` then
    `STRING` are tried directly (the hint can't be checked then).
  - Writing: clipGPT re-executes itself (`current_exe`) with the hidden
    argument `--x11-clipboard-owner`, in its own process group (so Ctrl-C
    in the starting terminal doesn't end it), with stderr discarded and
    cwd `/`, and passes the text on stdin. The helper takes `CLIPBOARD`
    with a real server timestamp, verifies ownership, reports `ok` or
    `error: <message>` on stdout (the parent waits at most 5 s, then
    kills it), and serves `TARGETS`, `TIMESTAMP`, `UTF8_STRING`,
    `text/plain;charset=utf-8` and `TEXT`. Texts above the server's
    request size (max 256 KiB per chunk) are sent with `INCR`, with
    several transfers at once supported. It exits on `SelectionClear`
    once all its transfers are done. Re-executing avoids `fork()` in a
    multi-threaded process and needs no `unsafe` or libc code.
- `src/cli.rs`: new `Command::ClipboardOwner`, recognised only when
  `--x11-clipboard-owner` is the sole argument, and kept out of `--help`.
  `main.rs` handles it before loading the config and never beeps in that
  mode.
- The decisions in the clipboard code are pure functions with unit tests,
  separate from the X11/Wayland I/O: Wayland result -> fallback/empty/error
  (`finish_get`/`finish_set`), capped pipe reads (`read_capped`), poll
  timeouts (`wait_until`), `TARGETS` parsing and the read plan
  (`target_atoms`, `plan_read`), chunk intake (`take_chunk`), the owner's
  answer to each request (`decide_answer`, `reply_property`), `INCR`
  bookkeeping for several requestors (`Transfers`), the helper command line
  (`helper_command`) and the helper's status protocol (`run_owner`).
- Tests: unit tests for fallback classification, `DISPLAY` detection,
  combined error messages, target choice, Latin-1/UTF-8 decoding and size
  limits, `INCR` reassembly and chunking, the helper status protocol and
  handover (with a shell stand-in for the helper: data passing, early
  exit, timeout), and the hidden argument. Three X11 round-trip tests
  (small, 300 KB `INCR`, over-limit) are `#[ignore]`d by default; run them
  with `xvfb-run -a cargo test -- --ignored`.
- Comment updates in `.woodpecker/release.yaml` and
  `scripts/ci-freebsd.sh` (the FreeBSD dependency closure never included
  libxcb).

## v0.3.0 - Security hardening: password detection, output cleaning, safer transport

User-facing changes:

- **Passwords are no longer sent to the LLM.** If a password manager marked
  the clipboard as secret (Wayland and Windows), clipGPT replaces the
  clipboard with `[clipGPT] Clipboard held a password - not sent to the
  LLM.`, plays the error tone, and sends nothing. Not available on macOS.
- **LLM output is cleaned before it's pasted**: control characters
  (including terminal escape sequences), bidirectional-override,
  zero-width, BOM and Unicode tag characters are removed, and line endings
  are normalised. Emoji joined with zero-width joiners come apart.
- **Secrets on the clipboard are not sent.** Text that looks like an API
  key or token (about 50 formats from 40 vendors), a private key, a JWT or
  a URL with a password is refused with the error tone; the clipboard is
  left untouched. Turn off with the new `-s`/`--no-secret-check` option or
  the new `secret_check=false` config key.
- **Hidden prompt injection is refused**: clipboard text containing
  invisible Unicode tag characters is replaced by a notice and nothing is
  sent (England/Scotland/Wales flag emoji are caught too).
- **Clipboard text is cleaned before sending** (the same invisible/control
  characters as the output).
- New command line options: `-s`/`--no-secret-check`, `-h`/`--help`.
  Unknown arguments are an error.
- **The prompt and the clipboard text are now sent as separate messages**
  (system and user), making prompt injection from clipboard text harder.
- **`api_key` is now optional.** Empty or missing means no key is sent
  (for servers like a local Ollama).
- **Behaviour change:** if `config.ini` contains an `api_key` and other
  users can access it (Unix mode wider than `0600`), clipGPT refuses to run
  and prints the `chmod 600` command. Existing setups with a `644` file
  will stop working until fixed.
- **Behaviour change:** HTTP redirects are no longer followed by default.
  New option `allow_redirects` (default `false`) turns them back on, with
  each hop checked.
- Plain `http` to a host other than this computer now prints a warning
  (the request still goes ahead). Non-http(s) URLs are refused.
- New option `max_input_bytes` (default 64 KiB): bigger clipboards are
  refused. Responses over 1 MiB are discarded.
- Errors no longer crash the program: every failure plays the error tone
  (now actually used) and exits with status 1. Error messages no longer
  include the server response, which could contain your clipboard text.

### Details

- New `src/sanitize.rs` (`sanitize_output`): strips `char::is_control`
  except `\n`/`\t`, U+202A-202E, U+2066-2069, U+200E/200F, U+061C,
  U+200B-200D, U+2060-2064, U+FEFF, U+E0000-E007F; then trims.
- New `src/net.rs`: `assess_url`/`check_url` (scheme and locality check),
  `ip_is_local`/`host_is_local` (loopback/unspecified, otherwise a UDP bind
  test against the address - std only, works the same on Linux, FreeBSD
  and Windows; multicast/broadcast never count as local; a name counts as
  local only if every address it resolves to is), `build_client` with
  `redirect::Policy::none()` or a custom policy running
  `redirect_decision` per hop (max 10), `post_json` (3xx/non-2xx report the
  status only; `Content-Length` and a `Read::take` cap enforce 1 MiB),
  `error_chain` for readable reqwest errors.
- `src/clipboard_backend.rs`: `get_text(max_bytes) -> Result<Clip, String>`
  with `Clip::Text`/`Clip::Password`; Wayland checks `get_mime_types`
  before reading, and reads through `take(max_bytes + 1)`; Windows opens
  the clipboard once, checks the registered marker formats, then checks
  the `CF_UNICODETEXT` size before copying. `set_text` returns `Result`.
  Marker matching is in pure functions with unit tests.
- `src/configfile.rs`: the file is read by clipGPT itself (pretty_ini
  panicked on unreadable/non-UTF-8 files) and the mode is taken from the
  same open handle; `api_key` is `Option<String>` (blank = `None`);
  `parse_bool` rejects unknown values so a typo can't turn on
  `allow_redirects`; `max_input_bytes` must be a positive number; an empty
  `model` is now an error too.
- `src/main.rs`: `run()` returns `Result<Outcome, String>`, with a
  `catch_unwind` fallback; `parse_response` reports only the serde error
  kind and position; `content: null` and empty `choices` are errors; an
  empty cleaned response leaves the clipboard unchanged. The
  `Authorization` header is sent only when a key is set.
- New `src/secrets.rs`: one `regex::RegexSet` (new dependency `regex`
  1.13, pure Rust - no platform backend), compiled once; `\b` rewritten to
  an ASCII word boundary so non-ASCII text doesn't force the slow matcher.
  `find_secret` returns only the kind. Test fixtures are assembled at
  runtime so the public repo holds no scanner-flaggable key strings; a test
  also runs the detector over `LICENSE`, `README.md` and
  `config.ini.example` to catch false positives.
- New `src/cli.rs`: hand-rolled `std::env::args` parsing.
- `src/sanitize.rs`: `sanitize_input` (cleans, doesn't trim) and
  `contains_tag_chars`; `src/main.rs` `screen_input` runs tag refusal,
  cleaning, then secret detection on the cleaned text (so a key split by
  zero-width characters is still caught).
- `src/beep.rs`: no panics; duration 0 skips the audio device entirely.
- Unit tests added across all modules (89), including loopback HTTP
  server tests for redirects, key handling and body hiding.

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
