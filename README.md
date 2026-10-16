

# configuration parameters:

| config file | command line | default value | type | comment |
| --- |  --- |  --- |  --- | --- |
| name |   | *section name*  | str | name which is used to execute this section when called from command line set to "" to disable |
| include   |   | | str  | include values from other sections of config file, values from root sections are always included |
| api_type |  | OpenAI | str |  Only OpenAI is curenetly supported which can be used with olama too |
| api_url |  | https://api.openai.com/v1/chat/completions | str| |
| api_key | -k |  | str | Must be provided via config file or command  line option |
| model |  |  | str | Must be provided via config file. Model name sent to api_url, e.g. gpt-4o-mini for OpenAI, or a local model name for Ollama |
| prompt_text | -t | "Check spelling and gramma and rewrite following text on pirate speak. Be mean and pushy \n\n" | str | prompt used if clipboard contains a text |
| prompt_image | -i | "OCR attached image and return text formatted as markdown"  |  str | prompt used if clipboard contains an image (not implemented) |
| beep_tone |  | 1046 | int | frequency in Hz, played when response is received and pasted to clipboard aprox. C6 |
| error_tone |   | 131  | int  | frequency in Hz, played when error occur aprox. C3 |
| beep_duration |  | 250 | int | duration in ms, for error and success beeps (set to 0 to disable)    



Command line only option  "-c" where to find configuration file.   
Defaults:   
* Linux *"~/.config/clip_gpt/config.ini"*
* MacOS  *"~/Library/Application Support/clip_gpt/config.ini"*  
* Windows *"%APPDATA%\clip_gpt\config.ini"*  %APPDATA% typically resolves to C:\Users\<username>\AppData\Roaming.

if config.ini not found on path specified above, current folder will be checked too.


# compiling

apt install libasound2-dev libxcb-xfixes0-dev libxcb-shape0-dev


sudo docker run -ti --name rust -v ".:/code" rust:1.73.0-bookworm
adduser --disabled-password cabinboy

[DEB guide](https://medium.com/rust-programming-language/simplifying-debian-packaging-for-rust-a-step-by-step-guide-for-rust-developers-0457cdb3c81d)

cargo install cargo-deb
cargo deb

# win

rustup target add x86_64-pc-windows-gnu
apt install mingw-w64
cargo build --target x86_64-pc-windows-gnu

#macOS

cargo install cross --locked
rustup target add aarch64-apple-darwin
