# Installing otto

Every way to install it, and what to do after an upgrade.

otto runs on macOS and Linux, on arm64 and x64, and [experimentally on Windows](windows.md). Every way below installs the same native binary; the command is `otto`.

**Homebrew**, on macOS and Linux:

```sh
brew install tarcisiopgs/tap/otto
```

**npm:**

```sh
npm install -g @tarcisiopgs/otto
```

Node is only used to start the binary. A scheduled job calls it by its full path, inside the folder npm installed the package in. That path changes when you upgrade otto through a Node version manager or switch Node versions, so run `otto sync` after either.

**Debian and Ubuntu:** releases after 0.1.0 attach a `.deb` for amd64 and arm64. Download the one for your machine from the [latest release](https://github.com/tarcisiopgs/otto/releases/latest) and install it:

```sh
sudo apt install ./otto_*.deb
```

No apt repository is involved, so a new version is installed the same way.

**A binary by hand:** each release also attaches a `.tar.xz` per platform, with its checksum.

After an upgrade by any of these, run `otto sync`: it rewrites the units when what they should contain has changed.

## Build from source

```sh
cargo build --release
cargo test
```

Requires Rust 1.88 or newer.
