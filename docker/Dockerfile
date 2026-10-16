FROM rust:1.92-trixie
RUN useradd -d /code -c "The User" -M user
RUN apt update && apt install -y libasound2-dev
# RUN apt install -y libpango1.0-dev
# RUN apt install -y libgtk-4-dev
# RUN apt install -y libadwaita-1-dev
USER user
# RUN cd ~ &&  cargo install cargo-deb

# cargo install cargo-deb
# cargo deb
# rustup target add x86_64-pc-windows-gnu
# apt install mingw-w64
# cargo build --target x86_64-pc-windows-gnu
