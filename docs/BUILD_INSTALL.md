# Build And Install

Waymark Shell is a Rust project. The public binary is `waymark`.

## Dependencies

- Rust toolchain from `rust-toolchain.toml`
- Cargo
- A POSIX-like host for the default shell/runtime path
- `python3` for the MCP adapter tests and helper scripts
- `git` for repository work and runtime state probes

Install the musl target before building release artifacts:

```sh
rustup target add x86_64-unknown-linux-musl
```

Depending on the host distribution, musl builds may also require a musl C toolchain
package such as `musl-tools`.

## Development Build

Use the default debug build while editing:

```sh
cargo build -p waymark
cargo test -p waymark
cargo test -p waymark-runtime --lib
```

The debug binary is:

```sh
target/debug/waymark
```

## Release Build

Use musl as the default release artifact:

```sh
cargo build --release -p waymark --target x86_64-unknown-linux-musl
```

The release binary is:

```sh
target/x86_64-unknown-linux-musl/release/waymark
```

A host-native release build can still be useful for local profiling or debugging:

```sh
cargo build --release -p waymark
```

## Install

Install by copying the release binary onto `PATH`:

```sh
install -Dm755 target/x86_64-unknown-linux-musl/release/waymark ~/.local/bin/waymark
```

Set `WAYMARK_START_DIR` when the process should begin in an explicit workspace:

```sh
WAYMARK_START_DIR="$PWD" waymark eval -c 'emit(state())'
```

For MCP adapter usage, set `WAYMARK_STONE_BIN` to the musl release binary unless
you are intentionally testing a debug build.
