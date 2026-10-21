# Release notes

This file lists user-facing changes for each tagged release. See `AGENTS.md`
for the versioning and tagging convention.

## v0.7.0 - Security hardening, request_extra_body, api_url required

**Breaking changes:**

- **`api_url` is now required** in every profile that handles input (set
  there or included), like `model`. There is no default server any more:
  a profile without `api_url` used to send the clipboard to OpenAI, now it
  is an error naming the profile. Add
  `api_url = "https://api.openai.com/v1/chat/completions"` to keep using
  OpenAI.
- **`keep_alive: "8h"` is no longer sent** with every request. To keep it
  for an Ollama server, add `request_extra_body = { keep_alive = "8h" }`
  to that profile.
- **New default `prompt_text`**: a plain spelling, grammar and punctuation
  fix that keeps the text's language, meaning, tone and formatting. The
  old default rewrote text "in pirate speak"; the short example config
  still does, on purpose.
- `request_timeout` and `--timeout` are at most 3600 (1 hour); 0 still
  means no limit.
- Redirects from `https` to plain `http` are refused, even with
  `allow_redirects = true`.
- Clipboard text with hidden variation selectors (any of U+E0100-U+E01EF,
  or two or more of U+FE00-U+FE0F in a row) is refused like tag
  characters. A single selector after a character, as in emoji, is fine.
- PNG and JPEG images whose structure can't be read are refused instead
  of being sent as they are.

User-facing changes:

- **New: `request_extra_body`**, a table of extra top-level fields for the
  JSON request body, for server options without a key of their own (e.g.
  Ollama's `keep_alive` or `options`). Not set by default.
- **Image metadata is removed** before sending: EXIF (camera, time, GPS
  position), XMP, IPTC, text chunks, comments, embedded extra images and
  anything after the end of the image. Colour profiles are kept. The EXIF
  orientation goes too, and an animated PNG is sent as its first frame.
- **More invisible characters are removed** from the text sent and the
  response pasted (soft hyphen, Hangul fillers, combining grapheme joiner,
  Mongolian vowel separator, interlinear annotation marks and others);
  line and paragraph separators U+2028/U+2029 become newlines.
- **Error messages no longer show the request URL's path or query**, where
  a key may be (reqwest added the full URL to its messages).
- **`--show-profile` hides secrets in `api_url`** (user name, password,
  query, fragment) and now checks the `api_url` of every selected profile,
  failing on an invalid one. A normal run only checks (and warns about)
  the URL of the profile actually used.
- The plain-http warning only mentions the API key when one is sent.
- **X11:** the password-manager marker is found among up to 4096 offered
  formats (was 256, which a padded list could get past). The background
  clipboard helper gives up on a paste that stalls for 10 seconds after
  something else was copied, instead of staying forever. The man page now
  also lists the Windows password markers and the X11 limits.
- **Fixed: on Wayland, the response (or the password/hidden-character
  placeholder) could vanish from the clipboard the moment clipGPT exited**
  - right after the success beep - unless something else (a clipboard
  manager, or another application) grabbed it first. Reproduced on a
  pure-Wayland session (sway, no Xwayland, no clipboard manager): the
  clipboard read back empty immediately after "Response copied to
  clipboard!". Wayland now gets the same kind of background clipboard
  owner X11 already had; the internal argument that starts it changed
  from `--x11-clipboard-owner` to `--clipboard-owner=x11` /
  `--clipboard-owner=wayland` (nothing to change in normal use - it is
  not meant to be run directly, and now refuses to if typed into a
  terminal by mistake).
- Release builds: FreeBSD dependency packages are verified against the
  repository's checksums, and every build uses `--locked`, so packages
  contain exactly the audited `Cargo.lock`.

### Details

- `net.rs`: `error_chain`/`describe` strip reqwest's " for url (...)"
  suffix (also inside `io::Error`s from body reads); `post_json` calls
  `without_url()`. `redirect_decision` takes the previous URLs and refuses
  an https -> non-https hop; the key is named in a hop's warning only if
  the origin is unchanged (reqwest drops `Authorization` otherwise).
  `check_url`/`insecure_warning`/`build_client` take whether a key is
  sent.
- `sanitize.rs`: `is_invisible_format` strip list; `visible_chars` flags
  hidden variation selectors (skipping characters stripped anyway, so a
  run split by zero-width characters still counts); `contains_tag_chars`
  became `contains_hidden_chars`. `main.rs`: `Block::HiddenTags` ->
  `Block::HiddenChars`, placeholder text "hidden characters".
- `imagefile.rs`: byte-level `strip_png` (keeps critical chunks and tRNS,
  gAMA, cHRM, sRGB, iCCP, sBIT, pHYs, cICP, bKGD; stops at IEND) and
  `strip_jpeg` (keeps APP0, APP2 ICC_PROFILE, APP14 Adobe, tables, frame
  and scan data, also between progressive scans; stops at EOI). No new
  dependency, no re-encoding; checked against ImageMagick baseline,
  progressive and CMYK JPEGs (pixel-identical).
- `configfile.rs`: `DEFAULT_API_URL` removed, `model` and `api_url` share
  a `required` check. `request_extra_body` (`KeyType::JsonTable`,
  `toml_to_json`; dates, nan/inf and the top-level keys model, messages,
  stream refused); `display_value` prints tables and floats.
  `MAX_REQUEST_TIMEOUT_SECS` = 3600. `display_api_url` for
  `--show-profile`. `Selection::handlers` yields (name, config).
- `main.rs`: `OpenAIRequest` flattens `request_extra_body`, no
  `keep_alive` field. `decide_text` returns Unhandled before any
  screening when no profile handles text. `run_with` checks only the used
  profile's `api_url`; `verify_api_urls` checks all for `--show-profile`.
- `clipboard_backend/x11.rs`: `MAX_TARGETS` 4096. INCR transfers record
  their last progress; after `SelectionClear` the owner polls and drops
  transfers idle for `TRANSFER_IDLE_TIMEOUT` (10 s).
- The background-owner protocol (spawn a copy of clipGPT with a hidden
  argument, hand it the text on stdin, read back a one-line status, leave
  it running once it says `ok`) moved out of `x11.rs` into
  `clipboard_backend/owner.rs`, shared with a new Wayland implementation:
  `set_text` now starts `--clipboard-owner=wayland` instead of calling
  wl-clipboard-rs's background `copy()` directly; the helper takes the
  clipboard with `Options::foreground(true).prepare_copy(..)` and then
  calls `PreparedCopy::serve()` itself, falling back to the X11 helper on
  the same errors (`MissingProtocol`/`WaylandConnection`/
  `SocketOpenError`) the old background-copy path did. The status line
  gained a third case, `unavailable: <message>`, for that fallback
  decision, distinct from a backend that was reached but failed
  (`TakeFailure`/`HandOverResult`). `run_owner_on_stdio` refuses to start
  if stdin is a terminal and caps how much text it reads (twice
  `net::MAX_RESPONSE_BYTES`), used by both backends' real entry points.
  Windows and macOS are unaffected: their clipboards hold data
  themselves, so nothing needs to keep running. Checked on a throwaway
  Proxmox VM (Debian 13, sway headless, pure Wayland): the response
  survives after clipGPT exits, and the helper exits once something else
  is copied; also checked that the X11 round trip still works end to end
  (`xclip` under `Xvfb`) under the renamed argument.
- `scripts/fetch-freebsd-deps.py`: each `.pkg` checked against
  `pkgsize` and `sum` (pkg's "2$" base32 BLAKE2b-512, or hex SHA-256)
  before unpacking; unknown or missing sums are errors. Docstring cleaned
  of references to other projects; User-Agent `clipgpt-ci`.
- `.woodpecker/release.yaml`, `scripts/ci-freebsd.sh`: `--locked` on all
  builds; header lists every download and how it is verified (llvm-mingw
  is only protected by HTTPS - no checksum is published).
- Docs: README, man page and examples updated throughout; README and man
  page config examples set `api_url`, and a new test parses them.
- Tests: 244 unit tests plus 5 X11 tests under `xvfb-run` (password
  marker behind 1000 padding entries, stalled INCR paste).

## v0.6.0 - Config profiles (config.toml), routing by clipboard content, -c

**Breaking change: `config.ini` is no longer read.** The configuration is
now `config.toml` (TOML) with named profiles. Without `-p`, the profile
`[default]` is used; if there is none, running without `-p` is an error.
To convert an old config, rename `[openai]` to `[default]` (or always run
with `-p openai`) and quote the string values:

```toml
# old config.ini            # new config.toml
# [openai]                  [default]
# model=llama3              model = "llama3"
# prompt_text=Fix it.\n     prompt_text = "Fix it.\n"
# allow_redirects=yes       allow_redirects = true
# request_timeout=120       request_timeout = 120
```

Other behaviour changes from the stricter format:

- Booleans are only `true`/`false` (no yes/no, 1/0, on/off).
- `\n` is a newline only inside "double-quoted" strings; 'single-quoted'
  strings take no escapes.
- Invalid `beep_tone`, `error_tone` or `beep_duration` values are now
  errors instead of silently falling back to the default.
- Unknown keys (keys are case-sensitive), wrong value types and settings
  outside a profile are errors - in every profile of the file, used or not.

User-facing changes:

- **New: profiles.** Every `[name]` table is a profile. `include = ["a",
  "b"]` starts from other profiles' settings (later includes win, the
  profile's own keys win over all of them). `clear = ["key", ...]` drops
  included values so the built-in default applies again; `key = ""` is a
  real empty string (`api_key = ""` means no key).
- **New: `input = [...]`** says which clipboard content a profile handles:
  `"text"`, `"image"`, or `["none"]` for a profile that only exists to be
  included (e.g. shared beep settings). Without `input` a profile handles
  every kind. `input` is inherited through `include` like any other key.
- **New: `-p NAME[,NAME...]` / `--profile`** picks profiles. Each kind of
  clipboard content goes to the last listed profile that accepts it, e.g.
  `-p default,ocr` sends text to `[default]` and images to an images-only
  `[ocr]`. Only kinds some selected profile handles are read, so with only
  image profiles selected an image is used even when text is on the
  clipboard too. Content no selected profile accepts is refused with the
  error tone and the clipboard is left unchanged. A selected profile left
  with nothing to handle prints a warning; selecting a `none` profile is an
  error.
- **New: `-c FILE` / `--config FILE`** reads exactly that config file
  instead of searching `~/.config/clipgpt/config.toml` and `./config.toml`
  (no fallback if it is missing).
- **New: `--list-profiles`** prints the profile names; **`--show-profile`**
  prints the config file used, the selected profiles' settings after
  includes with where each came from, and which profile handles which kind
  of content. The API key is never shown.
- If any profile holds an `api_key`, the file must be mode 0600 (as before,
  now for the whole file).
- Three example configs: `config.toml.example` (short),
  `config.expanded.toml.example` (several profiles with includes) and
  `config.full.toml.example` (every key with its default). The man page is
  the full reference; Windows users see the README on GitHub.
- A non-UTF-8 command line argument is now an "Unknown option" error instead
  of a crash.

### Details

- `Cargo.toml`: `pretty_ini` replaced by `toml` 1.1 (`std`, `parse`,
  `preserve_order`, `serde` features; parsed into a `toml::Table`, no serde
  derive). New in `Cargo.lock`: `toml`, `toml_parser`, `toml_datetime`,
  `toml_writer`, `serde_spanned`, `serde_core`, `winnow`, `hashbrown`.
- `configfile.rs` rewritten: `parse_file` checks each profile's keys
  against `KEYS` and value types (`KeyType`), profile names (letters,
  digits, `_`, `-`), and resolves every profile's includes up front
  (unknown profiles and cycles reported with the chain). TOML syntax errors
  report only the line and toml's short message, never the line's content;
  no message quotes a setting value. `ConfigFile::select` resolves the
  selected profiles, rejects `none` ones, assigns each `InputKind` to the
  last profile accepting it, and builds a `Config` only for profiles that
  handle something (only those need `model`). `ConfigFile::show` renders
  `--show-profile`. `load(Option<&Path>)` loads the `-c` file or searches;
  without a `config.toml` but with an old `config.ini`, the error says it
  is no longer read.
- `clipboard_backend.rs`: `Limits` fields became `Option<usize>` (`None` =
  kind not read); a shared `choose()` decides what to read on Wayland, X11,
  Windows and macOS, and `Clip::Unwanted(kind)` reports content no profile
  handles without reading it.
- `main.rs`: runs with the profile handling the clipboard's kind; every
  handler's `api_url` is checked up front; `-s` and `--timeout` apply to
  whichever profile handles the input; errors before the kind is known
  beep with the last selected profile's tones. `Block::Unhandled` names
  the selected profiles.
- `cli.rs`: arguments parsed as `OsStr` (`args_os`) so `-c` paths need not
  be UTF-8; `-p` takes a comma list only (twice, empty or repeated names
  are errors); `--list-profiles` and `--show-profile` cannot be combined.
- Man page: new PROFILES section, the new options and keys, examples for
  multi-profile configs and `-c`. README and AGENTS.md updated.
- Tests: 211 unit tests (plus 3 X11 tests under `xvfb-run`), covering
  parsing, includes, `clear`, `input`, routing, errors and their
  no-value-leak guarantee, the three example configs, `-c`, and non-UTF-8
  arguments and paths.

## v0.5.0 - Clipboard images (OCR) and an empty-clipboard fix

User-facing changes:

- **New: clipboard images are sent to the model.** When the clipboard
  holds an image and no text (e.g. a screenshot), clipGPT sends the image
  with `prompt_image` and puts the answer on the clipboard. When both text
  and an image are offered (office suites, browsers), the text wins.
- **New default `prompt_image`** makes the model act as an OCR engine:
  transcribe all text in the image exactly as written, laid out as
  Markdown, with no comments; `[illegible]` for unreadable parts and
  `[no text found]` for images without text.
- **New option `max_image_bytes`** (default 10485760, 10 MiB): bigger
  images are refused with the error tone.
- PNG and JPEG are sent as they are; WebP, GIF, BMP, Windows bitmaps and
  TIFF are converted to PNG, so llama.cpp, Ollama and OpenAI can all read
  them. Other image formats are refused.
- **The model must understand images** - choosing one is up to you. If the
  server or model refuses an image (e.g. llama.cpp started without
  `--mmproj`), the error tone plays and the message adds a hint that the
  model or server may not support image input.
- **Fixed: an empty clipboard (or one holding only an image) used to be
  sent as an empty message,** and the model's reply ("please provide the
  text...") replaced the clipboard with the success beep. A clipboard with
  no usable text and no image now plays the error tone and is left
  unchanged; so is text that is only white space or invisible characters.
- **macOS:** the clipboard is now read and written through the system
  pasteboard directly. This adds images (PNG, JPEG, TIFF) and detection of
  password-manager markers (`org.nspasteboard.ConcealedType`) on macOS.
  macOS is still not built by CI and this code has not been run on a Mac.
- **New option `-V`/`--version`** prints the version (`clip_gpt version 0.5.0`).

### Details

- `imagefile.rs` (new): `ClipImage::{File, Dib}` from the backends,
  `prepare` sniffs the format from its magic bytes (the clipboard's type
  name is never trusted), passes PNG/JPEG through, and decodes
  WebP/GIF/BMP/TIFF/DIB with the `image` crate (only its pure-Rust
  `png`, `gif`, `webp`, `bmp`, `tiff` codecs, no default features) and
  re-encodes them as 8-bit RGB or RGBA PNG. Decoding is bounded by
  `image::Limits`: 8192x8192 pixels and 256 MiB allocation, so a small
  file with huge declared dimensions is refused without allocating. The
  size limit is checked on the raw bytes and again on the converted PNG.
  `Prepared::data_url` encodes with the `base64` crate.
- `clipboard_backend.rs`: `get_text(max_bytes)` became `get(Limits { text,
  image })`, and `Clip` gained `Image(ClipImage)`. All backends check the
  password marker first, read text, and read an image only if the text is
  blank (`use_text`).
  - Wayland: the offered MIME types pick the image type (`image/png`,
    `jpeg`, `webp`, `gif`, `bmp`, `tiff`, in that order); a clipboard
    offering an image but no text type no longer counts as empty.
  - X11: `plan_read` returns the text targets and an image target from
    `TARGETS`; the image is read through the same chunked (`INCR`) reader,
    capped at `max_image_bytes` with an image-specific error.
  - Windows: registered `PNG`/`image/png` formats first, else `CF_DIB`;
    the size is checked before copying.
  - macOS: the unmaintained `clipboard` crate is replaced by
    `objc2-app-kit`'s `NSPasteboard`. `plan_macos` (unit-tested on every
    platform) checks `org.nspasteboard.ConcealedType`, then
    `public.utf8-plain-text`, then `public.png`/`public.jpeg`/
    `public.tiff`. Writing clears the pasteboard and sets the string.
    Only compile-checked (`cargo clippy --target aarch64-apple-darwin`).
- `main.rs`: `Message.content` is an untagged `Content::{Text, Parts}`,
  so text requests serialise exactly as before; images are sent as
  `[{"type":"image_url","image_url":{"url":"data:<mime>;base64,..."}}]`
  with `prompt_image` plus `IMAGE_DATA_NOTICE` (treat text in the image
  as data) as the system message. New `Block::Empty`, raised when the
  cleaned text is blank. Image requests pass a status hint to
  `net::post_json`, which appends it in parentheses to non-2xx errors
  (the response body is still never shown). llama.cpp answers a request
  with an image but no vision projector with HTTP 500 ("image input is not
  supported"), OpenAI typically with 400.
- `configfile.rs`: `max_image_bytes` (positive size, like
  `max_input_bytes`); `prompt_image` is no longer reserved. Tests check
  that `config.ini.example` carries the default prompt verbatim and that
  the INI parser keeps it whole.
- Dependencies: added `image` 0.25 and `base64` 0.23 (all targets) and
  `objc2`/`objc2-foundation`/`objc2-app-kit` (macOS only); removed
  `clipboard` 0.5, which drops `xcb`/`x11-clipboard` from `Cargo.lock`
  entirely, so `.cargo/audit.toml` and its RUSTSEC-2021-0019 exception are
  gone. Still builds with the minimum Rust version, 1.92.
- Docs: README "Images" section (formats, limits, vision models,
  llama.cpp `--mmproj`/`-hf`, timeouts, OCR-only models), man page
  `IMAGES` section and a llama.cpp example, `config.ini.example`.
- `cli.rs`: `Command::Version` for `-V`/`--version`; `version_text()`
  takes the version from `CARGO_PKG_VERSION` at build time, so it always
  matches `Cargo.toml`. The `--help` text mentions images and lists the new
  option.

## v0.4.1 - Audible beeps after the sound device has been idle

User-facing changes:

- **Fixed: the success/error beep was often silent or just a click.**
  Sound servers (e.g. PipeWire/WirePlumber) suspend the output after a few
  seconds of silence, and clipGPT always beeps after such a pause while it
  waits for the LLM. Waking the device swallowed the start of the sound, so
  a 250 ms beep was lost. Every beep now starts with 250 ms of silence, so
  it plays about 250 ms later than before but is audible.
- Problems during playback are now reported as "Could not play beep: ..."
  instead of being ignored silently.

### Details

- `beep.rs`: every beep is laid out as `LEAD_IN_MILLIS` (250) of silence,
  the tone with a 5 ms fade-in/out, then silence. Measured on a USB sink
  behind `pipewire-alsa` after 12 s of silence: no lead-in gave a click,
  150 ms and more gave the full tone, reproducibly; once the device was
  awake every variant played.
- The stream is kept open until cpal's playback clock (`Stream::now()`)
  passes the reported playback time of the last tone frame plus 50 ms,
  instead of sleeping for the tone length and dropping the stream (a drop
  halts output immediately). Bounded by a 2 s timeout past the expected
  end, so a stalled device or broken timestamps cannot hang the program.
- The stream's error callback forwards errors over a channel instead of
  discarding them; they become the `Could not play beep: ...` message.
- The device's default buffer size is used. cpal 0.18's ALSA backend
  already limits it to two periods and starts at two periods, so the
  start threshold was never the problem.
- Tests: tone layout, fade envelope, sample generation (silence before and
  after the tone), end-of-tone playback time and the finish check, all
  without an audio device.
- No config keys, options or defaults change.

## v0.4.0 - Configurable request timeout

User-facing changes:

- **New option `request_timeout`** (seconds, default 30, `0` = no limit):
  how long to wait for the LLM's answer. Before, the 30 s limit was built
  in, so slow local models (e.g. an Ollama model loading cold) failed with
  "operation timed out".
- **New command line option `--timeout SECONDS`** (also
  `--timeout=SECONDS`) overrides it for one run. There is no short form.
- A timeout is now reported clearly: "The LLM did not answer within 30 s;
  raise request_timeout in config.ini or use --timeout SECONDS (0 = no
  limit)".
- **Packages now include a man page**: `man clip_gpt` describes every
  option, config key, the clipboard behaviour and exit status.
- **Behaviour change (Debian/Ubuntu `.deb` only):** the binary moved from
  `/usr/local/bin/clip_gpt` to `/usr/bin/clip_gpt`, as Debian policy
  requires. Hotkeys that run plain `clip_gpt` keep working; ones that use
  the full old path need updating. FreeBSD is unchanged
  (`/usr/local/bin/clip_gpt`).

### Details

- `configfile.rs`: `Config::request_timeout: Option<Duration>` (`None` = no
  limit); `parse_timeout` accepts a whole number of seconds, anything else
  (negative, fractional, units, text) is an error naming the key; a
  missing or empty value means 30. The default matches reqwest's own
  default, which applied implicitly before.
- `cli.rs`: `Options::timeout`, parsed with the same `parse_timeout`; a
  missing or invalid value is an error with the usage text; the last one
  given wins. Long form only (`-t` is reserved for the planned prompt
  option).
- `net.rs`: `build_client` sets the timeout explicitly (covering the whole
  request including the body). `post_json` and `read_capped` report
  timeouts through `timeout_error`, which names the limit (with no limit,
  the underlying error is shown). Detected both from `send()`
  (`reqwest::Error::is_timeout`) and while reading the body (the reqwest
  timeout wrapped in the `io::Error`, or `ErrorKind::TimedOut`).
- `main.rs`: the command line value replaces the config value for the run.
- New `man/clip_gpt.1` (mdoc, `mandoc -T lint -W warning` clean). `.deb`:
  `/usr/share/man/man1/clip_gpt.1.gz` (cargo-deb gzips it); FreeBSD
  `.pkg`: `/usr/local/share/man/man1/clip_gpt.1.gz` (`gzip -9n` in
  `scripts/ci-freebsd.sh`, reproducible). Tests check that the page lists
  every option, every key in `config.ini.example` and the code's defaults.
- `.deb` binary path `usr/local/bin` -> `usr/bin` (Debian Policy 9.1.2,
  followed by Ubuntu: packages must not install into `/usr/local`). This
  clears lintian's `file-in-usr-local`/`dir-in-usr-local` errors, which the
  non-fatal lintian step in CI had hidden. Remaining lintian findings are
  unchanged (`malformed-contact`, `no-changelog`, `unknown-section`).
- CI: every `.deb` job checks for `usr/bin/clip_gpt` and the man page;
  `ci-freebsd.sh` checks the `.pkg` for the man page.
- Tests: timeout parsing (config and command line forms, errors), a local
  server that answers too late and one that stalls mid-body (both give the
  timeout message after 1 s), a slow answer with no limit, and the
  `io::Error` timeout detection.

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
