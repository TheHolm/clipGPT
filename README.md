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

2. Copy `config.ini.example` to `config.ini` and fill in `model`
   (required) and `api_key` (if your server needs one - a local Ollama
   doesn't). Place it at one of the locations listed under "Config file
   location" below. If it contains an `api_key`, make it private:
   `chmod 600 config.ini` (see "Security" below).

3. Run `target/release/clip_gpt`. It copies whatever text (or, failing
   that, image) is currently on the clipboard, sends it to the configured
   LLM, and replaces the clipboard contents with the response. Bind this to a hotkey/macro key using your
   OS's own tools so you don't have to run it manually every time.

## Configuration parameters

| config file | command line | default value | type | comment |
| --- |  --- |  --- |  --- | --- |
| api_type |  | OpenAI | str | Only OpenAI is currently supported (works with Ollama too, since it exposes an OpenAI-compatible API) |
| api_url |  | https://api.openai.com/v1/chat/completions | str | http or https only; plain http to another computer prints a warning (see "Security") |
| api_key | -k *(not yet implemented)* |  | str | Optional. Empty or missing means no key is sent (e.g. local Ollama). If set, the config file must be mode 600 on Unix |
| model |  |  | str | Required. Model name sent to api_url, e.g. gpt-4o-mini for OpenAI, or a local model name for Ollama |
| prompt_text | -t *(not yet implemented)* | "Check spelling and gramma and rewrite following text on pirate speak. Be mean and pushy \n\n" | str | prompt used if clipboard contains text (sent as the system message, the clipboard text as a separate user message) |
| prompt_image | -i *(not yet implemented)* | an OCR prompt (see `config.ini.example`): transcribe all text in the image exactly, as Markdown, nothing else | str | prompt used if the clipboard contains an image and no text (sent as the system message, the image as a separate user message). The model must accept images, see "Images" |
| beep_tone |  | 1046 | int | frequency in Hz, played when response is received and pasted to clipboard, approx. C6 |
| error_tone |   | 131  | int  | frequency in Hz, played when an error occurs or a password was found on the clipboard, approx. C3 |
| beep_duration |  | 250 | int | duration in ms, for error and success beeps (set to 0 to disable) |
| max_input_bytes |  | 65536 | int | largest clipboard text (UTF-8 bytes) sent to the LLM; bigger clipboards are refused |
| max_image_bytes |  | 10485760 | int | largest clipboard image (bytes) sent to the LLM, checked as read and after conversion to PNG; bigger images are refused |
| allow_redirects |  | false | bool | follow HTTP redirects from api_url (true/false, yes/no, 1/0, on/off) |
| secret_check | -s / --no-secret-check (turns it off) | true | bool | refuse clipboard text that looks like an API key, token, private key or URL with a password (see "Security") |
| request_timeout | --timeout SECONDS | 30 | int | longest wait in seconds for the LLM's whole answer; 0 = no limit. Raise it for slow local models (e.g. a cold Ollama model load) |

Command line options: `-s`/`--no-secret-check` skips the secret check for
one run; `--timeout SECONDS` (or `--timeout=SECONDS`) sets the
request timeout for one run, overriding `request_timeout` (0 = no limit);
`-V`/`--version` prints the version; `-h`/`--help` prints usage. Any other argument is an error.

The following are planned but **not yet implemented**: the `-c`/`-k`/`-t`/`-i`
command line options, and a `name`/`include` system for multiple named
profiles within one config file.

## Images

When the clipboard holds no text (or only white space) but an image,
clipGPT sends the image with `prompt_image` instead. When both are offered -
office suites and browsers often put text *and* a rendered picture of it on
the clipboard - the text wins. A clipboard with neither plays the error
tone and is left unchanged.

* **Formats.** PNG and JPEG are sent as they are. WebP, GIF (first frame),
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

Default per-OS location of `config.ini`:
* Linux *"~/.config/clipgpt/config.ini"*
* MacOS *"~/Library/Application Support/clipgpt/config.ini"*
* Windows *"%APPDATA%\clipgpt\config.ini"* - %APPDATA% typically resolves to `C:\Users\<username>\AppData\Roaming`.

If `config.ini` isn't found at the path above, the current working
directory is checked too (i.e. `./config.ini`).

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
  can't be detected.
* **Hidden prompt injection is refused.** Clipboard text containing
  Unicode tag characters (U+E0000-U+E007F - invisible, but read by LLMs)
  is treated as an attack: the clipboard is replaced with a notice, the
  error tone plays, nothing is sent. This check can't be turned off. The
  England/Scotland/Wales flag emoji use these characters and are refused
  too.
* **Clipboard text is cleaned before it is sent**, the same way as the
  response below (control, bidirectional-override, zero-width and BOM
  characters removed). Always on.
* **Secrets are not sent.** The cleaned text is scanned for API keys and
  tokens: private keys (PEM, age), OpenAI, Anthropic, OpenRouter, Groq, xAI,
  Hugging Face, Replicate, Perplexity, AWS, Google, Azure, DigitalOcean,
  Databricks, Vault, Doppler, GitHub, GitLab, npm, PyPI, Slack, Stripe,
  SendGrid, Shopify, Atlassian, Linear, Notion, Twilio, Mailgun, Mailchimp,
  Postman, Grafana, Sentry, Square, Telegram, Discord, JSON Web Tokens, and
  URLs with a password (a `user:password@` part after the scheme). On a
  match the error tone plays, nothing is sent and **the clipboard is left
  untouched**; stderr names only the kind of secret. Turn it off for one run with
  `-s`/`--no-secret-check`, or permanently with `secret_check=false`.
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
* **The response is cleaned before it is pasted.** Control characters
  (except newline and tab - this removes terminal escape sequences),
  bidirectional-override characters, zero-width characters, the BOM and
  Unicode tag characters are removed; line endings become `\n`. Emoji built
  with zero-width joiners fall apart into their parts as a side effect. The
  model can still write plausible but harmful plain text - read what you
  paste into a terminal.
* **Transport.** Only `http`/`https` URLs are accepted. Plain `http` to
  loopback or to one of this computer's own addresses (e.g. its LAN IP) is
  silent; to any other host it works but prints a warning on stderr, since
  the key and text travel unencrypted. Redirects are refused unless
  `allow_redirects=true`; then each hop is checked the same way (at most 10).
  When a redirect changes host the API key is dropped, but a 307/308
  redirect does re-send the clipboard text to the new host.
* **Limits.** Clipboard text over `max_input_bytes` and images over
  `max_image_bytes` are refused; responses over 1 MiB are discarded.
* **No content in logs.** Error messages contain only HTTP status codes and
  error kinds - never the response body, which can echo your text back.
* **Key file permissions.** On Linux/FreeBSD/macOS, if `config.ini` holds an
  `api_key` and other users can access it (mode wider than `0600`),
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
  * On X11 the clipboard holds no data itself: the program that copied
    something must keep running to hand it out. So after writing the
    response, clipGPT leaves a small background copy of itself (`clip_gpt
    --x11-clipboard-owner`) running. It exits as soon as anything else is
    copied. Don't run that option yourself; it's internal.
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

The X11 clipboard tests need a real X server, so `cargo test` skips them
by default. To run them headless:

```
apt install xvfb xauth
xvfb-run -a cargo test -- --ignored
```

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
