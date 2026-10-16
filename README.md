# clipGPT

A small command-line tool that grabs whatever text is on your clipboard,
sends it to an OpenAI-compatible LLM API (OpenAI itself, or a local server
like Ollama), and pastes the response back onto the clipboard. It's meant to
be bound to an OS-level hotkey or macro key so you can select some text,
trigger it, and have the rewritten/corrected version replace your clipboard
contents a moment later. A beep plays when the response is ready (or when an
error occurs).

**On Linux and FreeBSD, clipGPT is Wayland-only - X11 is not supported.**
See "Clipboard backend" below for why and what that means in practice.

# THIS IS VIBE CODED GARBAGE, USE ON YOUR OWN RISK

I did not check what is in the code at all, so who knows what it is really doing.

## Quick start

Prebuilt packages (Linux `.deb`, Windows `.exe`, FreeBSD `.pkg`) for each
tagged release are available on the
[GitHub Releases](https://github.com/TheHolm/clipGPT/releases) page - that's
the easiest way to get a working binary without compiling anything. To build
from source instead:

1. Build the release binary:
   ```
   cargo build --release
   ```
   The binary will be at `target/release/clip_gpt`.

2. Copy `config.ini.example` to `config.ini` and fill in `api_key` and
   `model` (both required - see the configuration table below). Place it at
   one of the locations listed under "Config file location" below.

3. Run `target/release/clip_gpt`. It copies whatever text is currently on
   the clipboard, sends it to the configured LLM, and replaces the clipboard
   contents with the response. Bind this to a hotkey/macro key using your
   OS's own tools so you don't have to run it manually every time.

## Configuration parameters

| config file | command line | default value | type | comment |
| --- |  --- |  --- |  --- | --- |
| api_type |  | OpenAI | str | Only OpenAI is currently supported (works with Ollama too, since it exposes an OpenAI-compatible API) |
| api_url |  | https://api.openai.com/v1/chat/completions | str | |
| api_key | -k *(not yet implemented)* |  | str | Required. Must be provided via config file |
| model |  |  | str | Required. Model name sent to api_url, e.g. gpt-4o-mini for OpenAI, or a local model name for Ollama |
| prompt_text | -t *(not yet implemented)* | "Check spelling and gramma and rewrite following text on pirate speak. Be mean and pushy \n\n" | str | prompt used if clipboard contains text |
| prompt_image | -i *(not yet implemented)* | "OCR attached image and return text formatted as markdown" | str | prompt used if clipboard contains an image (not implemented) |
| beep_tone |  | 1046 | int | frequency in Hz, played when response is received and pasted to clipboard, approx. C6 |
| error_tone |   | 131  | int  | frequency in Hz, played when error occurs, approx. C3 (not yet wired up) |
| beep_duration |  | 250 | int | duration in ms, for error and success beeps (set to 0 to disable) |

The following are planned but **not yet implemented**: the `-c`/`-k`/`-t`/`-i`
command line options, and a `name`/`include` system for multiple named
profiles within one config file.

### Config file location

Default per-OS location of `config.ini`:
* Linux *"~/.config/clipgpt/config.ini"*
* MacOS *"~/Library/Application Support/clipgpt/config.ini"*
* Windows *"%APPDATA%\clipgpt\config.ini"* - %APPDATA% typically resolves to `C:\Users\<username>\AppData\Roaming`.

If `config.ini` isn't found at the path above, the current working
directory is checked too (i.e. `./config.ini`).

## Clipboard backend

Clipboard access is OS-specific:

* **Linux and FreeBSD: Wayland only, via `wl-clipboard-rs`.** X11 is not
  supported. This was a deliberate choice, not an oversight: the previous
  backend (`clipboard`/`x11-clipboard`, built on the `xcb` crate) is
  unmaintained since 2022 and has real, published soundness advisories
  (e.g. [RUSTSEC-2021-0019](https://rustsec.org/advisories/RUSTSEC-2021-0019)).
  It's also an increasingly safe bet platform-wise: Ubuntu 25.10 (GNOME 49)
  already disables the X11 session by default, and Ubuntu 26.04 LTS (GNOME
  50) - the exact base this project's CI builds its `.deb` packages on -
  removed Xorg/X11 support entirely. If you're still on an X11-only desktop
  environment, clipGPT's clipboard access won't work there.
* **Windows**: `clipboard-win`, called directly.
* **macOS**: the old cross-platform `clipboard` crate, kept only for its
  Cocoa backend (the audit issue above was specifically in its X11 backend,
  not this one) - see the macOS note under "Releases" below.

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
own SDK. That said, clipGPT's dependencies (`clipboard`, `cpal`, `native-tls`)
all have genuine upstream macOS support, so building it natively on a real
Mac - just `cargo build --release`, no cross-compiling involved - will very
likely work too; it's just untested here, since no Mac hardware is available
in this environment.

## License

Licensed under the GNU Affero General Public License v3.0 or later
(AGPL-3.0-or-later). See [LICENSE](LICENSE) for the full text.
