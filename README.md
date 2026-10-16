# clip_gpt

A small command-line tool that grabs whatever text is on your clipboard,
sends it to an OpenAI-compatible LLM API (OpenAI itself, or a local server
like Ollama), and pastes the response back onto the clipboard. It's meant to
be bound to an OS-level hotkey or macro key so you can select some text,
trigger it, and have the rewritten/corrected version replace your clipboard
contents a moment later. A beep plays when the response is ready (or when an
error occurs).

## Quick start

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
* Linux *"~/.config/clip_gpt/config.ini"*
* MacOS *"~/Library/Application Support/clip_gpt/config.ini"*
* Windows *"%APPDATA%\clip_gpt\config.ini"* - %APPDATA% typically resolves to `C:\Users\<username>\AppData\Roaming`.

If `config.ini` isn't found at the path above, the current working
directory is checked too (i.e. `./config.ini`).

## Compiling

### Linux

```
apt install libasound2-dev libxcb-xfixes0-dev libxcb-shape0-dev
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

```
rustup target add x86_64-pc-windows-gnu
apt install mingw-w64
cargo build --target x86_64-pc-windows-gnu --release
```

### macOS (cross-compile from Linux)

```
cargo install cross --locked
rustup target add aarch64-apple-darwin
cross build --target aarch64-apple-darwin --release
```

## License

Licensed under the GNU Affero General Public License v3.0 or later
(AGPL-3.0-or-later). See [LICENSE](LICENSE) for the full text.
