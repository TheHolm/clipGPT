# clipGPT

A small command-line tool that grabs whatever text is on your clipboard,
sends it to an OpenAI-compatible LLM API (OpenAI itself, or a local server
like Ollama or llama.cpp), and pastes the response back onto the clipboard. It's meant to
be bound to an OS-level hotkey or macro key so you can select some text,
trigger it, and have the rewritten/corrected version replace your clipboard
contents a moment later. A beep plays when the response is ready (or when an
error occurs). If the clipboard holds an image instead of text - e.g. a
screenshot - it is sent to the model, which by default transcribes the text
in it as Markdown (see "Images" below).

**On Linux and FreeBSD, clipGPT prefers Wayland and falls back to X11**
(needed on GNOME, whose compositor doesn't offer the Wayland clipboard
protocol clipGPT uses). See "Clipboard backend" below for details.

# THIS IS VIBE CODED GARBAGE, USE ON YOUR OWN RISK

I did not check what is in the code at all, so who knows what it is really doing.

## Quick start

Prebuilt packages (Linux `.deb`, Windows `.exe`, FreeBSD `.pkg`) for each
tagged release are available on the
[GitHub Releases](https://github.com/TheHolm/clipGPT/releases) page - that's
the easiest way to get a working binary without compiling anything:

| package | binary | man page |
| --- | --- | --- |
| Debian / Ubuntu `.deb` | `/usr/bin/clip_gpt` | `/usr/share/man/man1/clip_gpt.1.gz` |
| FreeBSD `.pkg` | `/usr/local/bin/clip_gpt` | `/usr/local/share/man/man1/clip_gpt.1.gz` |

`man clip_gpt` describes every option and config key (source:
`man/clip_gpt.1`; read it unpackaged with `man -l man/clip_gpt.1`). Until
v0.3.1 the `.deb` installed to `/usr/local/bin`; update any hotkey that uses
that full path.

To build from source instead:

1. Build the release binary:
   ```
   cargo build --release
   ```
   The binary will be at `target/release/clip_gpt`.

2. Copy `config.toml.example` to `config.toml` and fill in `api_url` and
   `model` (both required, there is no default server) and `api_key` (if
   your server needs one - a local Ollama doesn't) in its `[default]`
   profile. `config.expanded.toml.example`
   shows several profiles with includes, `config.full.toml.example` every
   key with its default. Place it at one of the locations
   listed under "Config file location" below. If it contains an `api_key`,
   make it private: `chmod 600 config.toml` (see "Security" below).

3. Run `target/release/clip_gpt`. It copies whatever text (or, failing
   that, image) is currently on the clipboard, sends it to the configured
   LLM, and replaces the clipboard contents with the response. Bind this to a hotkey/macro key using your
   OS's own tools so you don't have to run it manually every time.

## Configuration

The full reference for the config file and how clipGPT uses it is the man
page (`man clip_gpt`, or `man -l man/clip_gpt.1`; sections CONFIGURATION
and PROFILES). This section is a summary.

**Since v0.6.0 the config file is `config.toml`** (TOML) with named
profiles; the old `config.ini` is no longer read. To convert one, rename
`[openai]` to `[default]` and quote the string values:

```toml
[default]
api_url = "http://127.0.0.1:11434/v1/chat/completions"
model = "llama3"
prompt_text = "Fix spelling and grammar.\n"
allow_redirects = false   # true/false only, no yes/no/1/0
request_timeout = 120
```

### Profiles

Every `[name]` table is a profile. Without `-p` clipGPT uses `[default]`
(an error if there is none); `-p NAME` or a comma-separated list
`-p a,b` picks others. Per profile:

* `include = ["a", "b"]` - start from these profiles' settings. Later
  includes override earlier ones; the profile's own keys override them all.
  Nested includes work; unknown profiles and cycles are errors.
* `clear = ["key", ...]` - drop these keys' included values, so the built-in
  default applies again. `key = ""` instead sets a real empty string
  (`api_key = ""` means no key, which also drops an included one).
* `input = [...]` - what the profile handles: `"text"`, `"image"`, or
  `["none"]` for a profile that only exists to be included (it can't be
  selected with `-p` and needs no `model` or `api_url`). Without `input` a
  profile handles every kind, including kinds added in later versions.
  `input` is passed on by `include` like any other key.

With several profiles, each kind of clipboard content goes to the **last**
listed profile whose `input` accepts it, and every setting of that run
comes from that profile. Only kinds some selected profile handles are read
from the clipboard, so with only image profiles selected an image is used
even if text is on the clipboard too. Content nobody accepts is refused with
the error tone. A listed profile left with nothing to handle only prints a
warning.

```toml
[beeps]            # shared beep settings, include-only
input = ["none"]
beep_tone = 880

[default]
include = ["beeps"]
clear = ["input"]  # don't inherit "none" from [beeps]
api_url = "https://api.openai.com/v1/chat/completions"
api_key = "sk-..."
model = "gpt-4o-mini"

[ocr]
include = ["beeps"]
input = ["image"]
api_url = "http://127.0.0.1:8080/v1/chat/completions"
model = "gemma-3-4b-it"
```

`clip_gpt -p default,ocr` sends text to OpenAI and images to the local
server; `-p ocr,default` sends everything to `[default]` (and warns that
`[ocr]` is unused). `clip_gpt -p default,ocr --show-profile` prints the
resulting settings, where each came from, and which profile handles what;
`--list-profiles` lists the profile names.

The whole file is checked on every run, including unused profiles: unknown
keys (case-sensitive), wrong value types, unknown or cyclic includes are
errors. Error messages never quote setting values.

### Keys

| config file | command line | default value | type | comment |
| --- |  --- |  --- |  --- | --- |
| include |  |  | list of str | profiles to start from, see "Profiles" |
| clear |  |  | list of str | included keys to reset to their built-in default |
| input |  | every kind | list of str | `"text"`, `"image"`, or `["none"]` |
| api_type |  | OpenAI | str | Only OpenAI is currently supported (works with Ollama too, since it exposes an OpenAI-compatible API) |
| api_url |  |  | str | Required in every profile that handles input; no default. Chat completions endpoint, e.g. https://api.openai.com/v1/chat/completions for OpenAI. http or https only; plain http to another computer prints a warning (see "Security") |
| api_key | -k *(not yet implemented)* |  | str | Optional. Empty or missing means no key is sent (e.g. local Ollama). If any profile sets one, the config file must be mode 600 on Unix |
| model |  |  | str | Required in every profile that handles input. Model name sent to api_url, e.g. gpt-4o-mini for OpenAI, or a local model name for Ollama |
| prompt_text | -t *(not yet implemented)* | "Correct the spelling, grammar and punctuation of the following text. Keep its original language, meaning, tone and formatting - do not translate, summarise or add anything. Reply with the corrected text only." | str | prompt used if clipboard contains text (sent as the system message, the clipboard text as a separate user message) |
| prompt_image | -i *(not yet implemented)* | an OCR prompt (see `man clip_gpt`): transcribe all text in the image exactly, as Markdown, nothing else | str | prompt used if the clipboard contains an image and no text (sent as the system message, the image as a separate user message). The model must accept images, see "Images" |
| beep_tone |  | 1046 | int | frequency in Hz, played when response is received and pasted to clipboard, approx. C6 |
| error_tone |   | 131  | int  | frequency in Hz, played when an error occurs or a password was found on the clipboard, approx. C3 |
| beep_duration |  | 250 | int | duration in ms, for error and success beeps (set to 0 to disable) |
| max_input_bytes |  | 65536 | int | largest clipboard text (UTF-8 bytes) sent to the LLM; bigger clipboards are refused |
| max_image_bytes |  | 10485760 | int | largest clipboard image (bytes) sent to the LLM, checked as read and after conversion to PNG; bigger images are refused |
| allow_redirects |  | false | bool | follow HTTP redirects from api_url (never from https to http) |
| secret_check | -s / --no-secret-check (turns it off) | true | bool | refuse clipboard text that looks like an API key, token, private key or URL with a password (see "Security") |
| request_timeout | --timeout SECONDS | 30 | int | longest wait in seconds for the LLM's whole answer, at most 3600 (1 hour); 0 = no limit. Raise it for slow local models (e.g. a cold Ollama model load) |
| request_extra_body |  |  | table | extra top-level fields for the JSON request body, e.g. `{ keep_alive = "8h" }` to keep an Ollama model loaded for 8 hours. Values keep their TOML type; dates, `model`, `messages` and `stream` are refused. Not set: nothing extra is sent. A profile's own table replaces an included one |

Strings are quoted (`"..."` takes escapes such as `\n`, `'...'` doesn't);
numbers and `true`/`false` are not.

Command line options: `-c`/`--config FILE` reads that config file instead
of searching the usual locations (no fallback if it's missing; use an
absolute path in hotkey bindings); `-p`/`--profile NAME[,NAME...]` picks the profiles
(see above); `--list-profiles` prints the profile names; `--show-profile`
prints the config file used and the selected profiles' resolved settings (API key hidden, and
`api_url` without user info, query or fragment) and exits,
which also checks the config, including every selected profile's
`api_url` (a normal run only checks the one it uses); `-s`/`--no-secret-check` skips the secret
check for one run; `--timeout SECONDS` (or `--timeout=SECONDS`) sets the
request timeout for one run, overriding `request_timeout` (0 = no limit);
`-s` and `--timeout` apply to whichever profile handles the clipboard;
`-V`/`--version` prints the version; `-h`/`--help` prints usage. Any other
argument is an error.

The `-k`/`-t`/`-i` command line options are planned but **not yet
implemented**.

## Images

When the clipboard holds no text (or only white space) but an image,
clipGPT sends the image with `prompt_image` instead. When both are offered -
office suites and browsers often put text *and* a rendered picture of it on
the clipboard - the text wins, unless no selected profile takes text (see
"Profiles"). A clipboard with neither plays the error
tone and is left unchanged.

* **Formats.** PNG and JPEG are sent without re-encoding, but with their
  metadata removed (see "Security"). WebP, GIF (first frame),
  BMP, Windows bitmaps (`CF_DIB`, e.g. from Print Screen) and TIFF (macOS
  screenshots) are converted to PNG first, because not every server can
  decode them (llama.cpp can't read WebP, OpenAI doesn't take BMP/TIFF).
  The format is recognised from the data itself, not from the name the
  copying program gave it. Anything else is refused.
* **Limits.** Images over `max_image_bytes` (default 10 MiB) are refused,
  both as read and after conversion. Conversion refuses pictures over
  8192 pixels wide or high, or needing more than 256 MiB to decode.
* **The model must understand images.** There's no check in advance -
  picking a vision model is up to you (e.g. `gpt-4o-mini` on OpenAI;
  `gemma3`, `qwen2.5vl` or `llama3.2-vision` on Ollama). If the server or
  model can't take images, the request fails with an HTTP error and the
  error tone; the message then adds a hint, e.g. `API returned HTTP 500 (the
  model or server may not support image input - ...)`. Some older Ollama
  versions ignore the image silently instead and answer anyway; clipGPT
  can't detect that.
* **llama.cpp.** `llama-server` accepts images only when started with the
  model's multimodal projector: `llama-server -m model.gguf --mmproj
  mmproj.gguf`, or `llama-server -hf ggml-org/gemma-3-4b-it-GGUF` (which
  downloads the projector too). Without it, image requests fail with HTTP
  500. Point `api_url` at `http://127.0.0.1:8080/v1/chat/completions`;
  `model` is required by clipGPT but llama-server serves whatever it loaded.
* **Speed.** Turning an image into model input is slow on a CPU, especially
  for the first request after loading. Raise `request_timeout` (or use
  `--timeout`) if requests time out.
* **OCR-only models** (PaddleOCR-VL, GLM-OCR, DeepSeek-OCR, ...) are trained
  on their own fixed prompts; set `prompt_image` to what the model expects.

### Config file location

Default per-OS location of `config.toml`:
* Linux/FreeBSD *"~/.config/clipgpt/config.toml"*
* MacOS *"~/Library/Application Support/clipgpt/config.toml"*
* Windows *"%APPDATA%\clipgpt\config.toml"* - %APPDATA% typically resolves to `C:\Users\<username>\AppData\Roaming`.

If `config.toml` isn't found at the path above, the current working
directory is checked too (i.e. `./config.toml`). `-c FILE` skips both and
reads exactly `FILE`.

## Security

clipGPT sends your clipboard to a remote service and puts untrusted model
output back where you will paste it, so it applies these protections:

* **Passwords are never sent.** If a password manager marked the clipboard
  contents as secret, clipGPT doesn't read them, replaces the clipboard with
  `[clipGPT] Clipboard held a password - not sent to the LLM.`, plays the
  error tone and makes no request. Detected markers: Wayland and X11
  `x-kde-passwordManagerHint` (KeePassXC, KDE and others); Windows
  `ExcludeClipboardContentFromMonitorProcessing`, `Clipboard Viewer Ignore`,
  or `CanIncludeInClipboardHistory` = 0; macOS
  `org.nspasteboard.ConcealedType` (1Password, KeePassXC, Bitwarden and
  others - untested, see "Releases"). A password manager that sets no marker
  can't be detected. On X11 the marker can only be seen in the list of
  formats the copying program offers (`TARGETS`): a program that doesn't
  answer that request is read anyway (as plain text, no image), and only
  the first 4096 entries of a longer list are checked.
* **Hidden prompt injection is refused.** Clipboard text containing
  Unicode tag characters (U+E0000-U+E007F - invisible, but read by LLMs)
  or hidden variation selectors is treated as an attack: the clipboard is
  replaced with a notice, the error tone plays, nothing is sent. This
  check can't be turned off. Hidden variation selectors are any of
  U+E0100-U+E01EF, and two or more of U+FE00-U+FE0F in a row ("emoji
  smuggling" hides data that way behind one visible character); a single
  one after a character (e.g. U+2764 U+FE0F, the red heart emoji) is
  normal and allowed. The England/Scotland/Wales flag emoji use tag
  characters and are refused too.
* **Clipboard text is cleaned before it is sent**, the same way as the
  response below (control, bidirectional-override, zero-width, BOM and
  other invisible characters removed). Always on.
* **Secrets are not sent.** The cleaned text is scanned for API keys and
  tokens: private keys (PEM, age), OpenAI, Anthropic, OpenRouter, Groq, xAI,
  Hugging Face, Replicate, Perplexity, AWS, Google, Azure, DigitalOcean,
  Databricks, Vault, Doppler, GitHub, GitLab, npm, PyPI, Slack, Stripe,
  SendGrid, Shopify, Atlassian, Linear, Notion, Twilio, Mailgun, Mailchimp,
  Postman, Grafana, Sentry, Square, Telegram, Discord, JSON Web Tokens, and
  URLs with a password (a `user:password@` part after the scheme). On a
  match the error tone plays, nothing is sent and **the clipboard is left
  untouched**; stderr names only the kind of secret. Turn it off for one run with
  `-s`/`--no-secret-check`, or permanently with `secret_check = false`.
  Placeholder connection strings in documentation (a URL with any
  username and password, even made-up ones) match too.
* **Prompt and clipboard are kept apart.** `prompt_text` goes in the system
  message (with a note to treat the user message as data); the clipboard
  text is the user message on its own. This makes prompt injection by
  clipboard text harder, not impossible.
* **Images aren't filtered.** Text inside an image can't be checked for
  secrets or hidden characters, so don't OCR screenshots of keys. The
  model is told to treat text in the image as data, not instructions -
  again harder, not impossible, to abuse. Password-manager markers are
  still honoured.
* **Image metadata is removed.** PNG and JPEG keep only what is needed to
  show the picture (colour profile, gamma, transparency, JFIF/Adobe
  headers); EXIF (camera, time, GPS position), XMP, IPTC, text chunks,
  comments, embedded extra images and anything after the end of the image
  are dropped. A JPEG whose structure can't be read is refused. Other
  formats are converted to PNG, which carries no metadata. A side effect:
  the EXIF orientation is dropped too, so a photo whose camera only marked
  it as rotated reaches the model unrotated; an animated PNG is sent as its
  first frame.
* **The response is cleaned before it is pasted.** Control characters
  (except newline and tab - this removes terminal escape sequences),
  bidirectional-override characters, zero-width characters, the BOM,
  Unicode tag characters, hidden variation selectors and other invisible
  format characters (soft hyphen, Hangul fillers, combining grapheme
  joiner, Mongolian vowel separator, interlinear annotation marks and
  similar) are removed; line endings, including U+2028/U+2029, become
  `\n`. Emoji built
  with zero-width joiners fall apart into their parts as a side effect. The
  model can still write plausible but harmful plain text - read what you
  paste into a terminal.
* **Transport.** Only `http`/`https` URLs are accepted. Plain `http` to
  loopback or to one of this computer's own addresses (e.g. its LAN IP) is
  silent; to any other host it works but prints a warning on stderr, since
  the key and text travel unencrypted. Redirects are refused unless
  `allow_redirects = true`; then each hop is checked the same way (at most
  10), and a redirect from `https` to plain `http` is always refused, even
  to this computer. A run checks (and warns about) only the `api_url` of
  the profile that handles the clipboard, once its kind is known;
  `--show-profile` checks those of all selected profiles.
  When a redirect changes host the API key is dropped, but a 307/308
  redirect does re-send the clipboard text to the new host.
* **Limits.** Clipboard text over `max_input_bytes` and images over
  `max_image_bytes` are refused; responses over 1 MiB are discarded.
* **No content in logs.** Error messages contain only HTTP status codes and
  error kinds - never the response body, which can echo your text back.
  URLs in messages are cut to scheme, host and port, so a key in the
  `api_url` path or query isn't printed either.
* **Key file permissions.** On Linux/FreeBSD/macOS, if `config.toml` holds
  an `api_key` in any profile and other users can access it (mode wider than `0600`),
  clipGPT refuses to run and prints the `chmod 600` command to fix it.
  Windows is not checked (`%APPDATA%` is per-user).

Every failure plays `error_tone` and exits with status 1.

## Clipboard backend

Clipboard access is OS-specific:

* **Linux and FreeBSD: Wayland first, X11 as fallback.**
  * **Wayland**, via `wl-clipboard-rs`, is used whenever the compositor
    supports the `ext-data-control` or `wlr-data-control` protocol (KDE
    Plasma, Sway, Hyprland and most other wlroots-based compositors). A
    tool without a window of its own can only reach the Wayland clipboard
    through these protocols.
  * **X11**, via `x11rb`, is used when Wayland clipboard access isn't
    available and `DISPLAY` is set:
    * **GNOME** (Mutter) implements neither data-control protocol, so
      clipGPT goes through Xwayland there. GNOME keeps the Xwayland and
      Wayland clipboards in sync, so this is the same clipboard.
    * **Pure X11 sessions** work the same way.
  * **Neither clipboard holds data itself**: the program that copied
    something must keep running to hand it out. So after writing the
    response, clipGPT leaves a small background copy of itself running
    (`clip_gpt --clipboard-owner=wayland` or `--clipboard-owner=x11`,
    whichever backend set the clipboard). It exits as soon as anything
    else is copied. On X11 that includes finishing pastes already in
    progress; one that stalls for 10 seconds is given up. The Wayland
    helper has no such limit yet: a stalled paste there can keep it
    running. Don't run that option yourself; it's internal.
  * `x11rb` is maintained and pure Rust: no libxcb, and in the default
    configuration used here it is compiled with `forbid(unsafe_code)`. It
    is unrelated to the `clipboard`/`x11-clipboard`
    crates this project used before, which were built on the `xcb` crate:
    unmaintained since 2022, with real, published soundness advisories
    (e.g. [RUSTSEC-2021-0019](https://rustsec.org/advisories/RUSTSEC-2021-0019)).
    That's why the old backend was dropped.
  * If neither works, the error names both reasons (e.g. "Wayland: ...;
    X11: ...").
* **Windows**: `clipboard-win`, called directly.
* **macOS**: the system pasteboard (`NSPasteboard`) directly, via the
  maintained `objc2` bindings. Unlike the old `clipboard` crate used before,
  this can list the pasteboard's types, which images and password-marker
  detection need - see the macOS note under "Releases" below.

## Compiling

### Linux

```
apt install libasound2-dev
cargo build --release
```

To build a `.deb` package instead, see this
[DEB guide](https://medium.com/rust-programming-language/simplifying-debian-packaging-for-rust-a-step-by-step-guide-for-rust-developers-0457cdb3c81d):

```
cargo install cargo-deb
cargo deb
```

The X11 clipboard tests, and the end-to-end ones under `tests/` that run
the real binary against it (`xclip` round trips through a local stub LLM
server), need a real X server, so `cargo test` skips them by default. To
run them headless:

```
apt install xvfb xauth xclip
xvfb-run -a cargo test -- --ignored
```

The `tests/cli_dispatch.rs` integration tests (`--help`, `--list-profiles`,
`--show-profile`, ...) need no display and run with plain `cargo test`.

### Docker (build environment only)

`docker/` contains a Dockerfile/compose setup for building the project in a
clean container - it's for compiling, not for running `clip_gpt` itself
(clipboard/audio access generally isn't available from inside a plain
container).

```
docker compose -f docker/docker-compose.yaml up -d
```

### Windows (cross-compile from Linux)

x86_64:
```
rustup target add x86_64-pc-windows-gnu
apt install mingw-w64
cargo build --target x86_64-pc-windows-gnu --release
```

aarch64 - needs [llvm-mingw](https://github.com/mstorsjo/llvm-mingw) (classic
GNU mingw-w64 has no ARM64 Windows support; the real rustc target is
`aarch64-pc-windows-gnullvm`, not `aarch64-pc-windows-gnu`):
```
rustup target add aarch64-pc-windows-gnullvm
# download and extract an llvm-mingw release, then add its bin/ to PATH
export CARGO_TARGET_AARCH64_PC_WINDOWS_GNULLVM_LINKER=aarch64-w64-mingw32-clang
export AR_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-ar
export CC_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-clang
cargo build --target aarch64-pc-windows-gnullvm --release
```

### FreeBSD (cross-compile from Linux)

No FreeBSD host is needed - see `.woodpecker/release.yaml` and `scripts/`
for the sysroot/dependency-closure approach used in CI (`scripts/ci-freebsd.sh`
ties it together, calling `scripts/fetch-freebsd-deps.py` and
`scripts/build-freebsd-pkg.py`).

## Releases

Pushing a tag `vX.Y.Z` matching the version in `Cargo.toml` triggers
`.woodpecker/release.yaml`, which builds Linux `.deb` (x86_64 and aarch64),
Windows `.exe` (x86_64 and aarch64) and FreeBSD `.pkg` (x86_64) artifacts and
publishes them to a GitHub release. See `AGENTS.md` for the versioning and
tagging convention.

macOS builds aren't produced by CI: cross-compiling for macOS needs Apple's
SDK, which Apple's license doesn't allow redistributing, so there's no
Docker image or CI setup that can produce one without you supplying your
own SDK. That said, clipGPT's dependencies (`objc2-app-kit`, `cpal`,
`native-tls`) all have genuine upstream macOS support, so building it
natively on a real Mac - just `cargo build --release`, no cross-compiling
involved - will very likely work too; it's just untested here, since no Mac
hardware is available in this environment. The macOS code is only checked
to compile (`cargo check --target aarch64-apple-darwin` needs no SDK);
clipboard text, images and password markers on macOS have never actually
been run.

## License

Licensed under the GNU Affero General Public License v3.0 or later
(AGPL-3.0-or-later). See [LICENSE](LICENSE) for the full text.
