# kiwix-cli

[English](README.md) | [简体中文](README.zh-CN.md)

[![Latest release](https://img.shields.io/github/v/release/joey5403/kiwix-cli?label=release)](https://github.com/joey5403/kiwix-cli/releases/latest) [![AUR package](https://img.shields.io/aur/version/kiwix-cli-bin?label=AUR)](https://aur.archlinux.org/packages/kiwix-cli-bin)

A keyboard-first Kiwix reader built with Rust, Ratatui, and Crossterm. It connects to the public Kiwix Browse service by default, or one self-hosted Kiwix server when configured, and supports library home pages, full-text search, random articles, styled article reading, internal links, and external image viewing from a terminal or SSH session.

## Usage

### Requirements

- A reachable Kiwix server
- A terminal for interactive mode
- Rust 1.88 or newer when building from source
- A system file opener for images, such as `xdg-open` on Linux

### Install

#### AUR

The official `kiwix-cli-bin` package is available in the AUR. Install it with an AUR helper:

```bash
yay -S kiwix-cli-bin
# or
paru -S kiwix-cli-bin
```

#### GitHub Release

Download and install the current Linux `x86_64` release:

```bash
VERSION=0.1.1
ARCHIVE="kiwix-cli-${VERSION}-linux-x86_64.tar.gz"
BASE_URL="https://github.com/joey5403/kiwix-cli/releases/download/v${VERSION}"

curl --fail --location --remote-name "${BASE_URL}/${ARCHIVE}"
curl --fail --location --remote-name "${BASE_URL}/${ARCHIVE}.sha256"
sha256sum -c "${ARCHIVE}.sha256"
tar -xzf "$ARCHIVE"
install -Dm755 "kiwix-cli-${VERSION}-linux-x86_64/kiwix-cli" "$HOME/.local/bin/kiwix-cli"
install -Dm644 "kiwix-cli-${VERSION}-linux-x86_64/man/man1/kiwix-cli.1" "$HOME/.local/share/man/man1/kiwix-cli.1"
```

Ensure `$HOME/.local/bin` is on `PATH` before starting `kiwix-cli`.

Build and install from this checkout:

```bash
cargo install --path .
```

Or build an optimized binary without installing it:

```bash
cargo build --release
./target/release/kiwix-cli --help
```

To build a Linux release archive containing the binary, license, bilingual README files, and a SHA-256 checksum:

```bash
./scripts/build-linux.sh
```

Install the manual page for local use:

```bash
install -Dm644 man/kiwix-cli.1 ~/.local/share/man/man1/kiwix-cli.1
man kiwix-cli
```

### Configure

The default server is:

```text
https://browse.library.kiwix.org/
```

Override it with `--server` or `KIWIX_URL` when using a self-hosted server:

```bash
export KIWIX_URL="https://wiki.example.com"
```

For a server protected by HTTP Basic Auth, set both credentials:

```bash
export KIWIX_USERNAME="wiki"
export KIWIX_PASSWORD="..."
```

The password has no command-line option, so it is not exposed through shell history or process listings. TLS certificates are verified. Redirects are not followed automatically; Kiwix home and random redirects are validated before use.

### Interactive mode

Start without a subcommand:

```bash
kiwix-cli
```

Select a library and press `Enter` to open its home page. Press `/` when you want to search that library.

| Key | Action |
| --- | --- |
| `j` / `k`, arrow keys | Move selection or scroll an article |
| `Enter` / `l` | Open a library home page, search result, or selected article action |
| `/` | Search the current library |
| `n` / `p` | Next / previous search result page |
| `g` / `G` | Move to the beginning / end |
| `Space` / `b` | Page down / page up in an article |
| `r` | Reload the current view |
| `R` | Open a random article from the current library |
| `Tab` / `Shift-Tab` | Select the next / previous article link or image |
| Mouse click | Open a link or image under the pointer |
| Mouse wheel | Scroll an article |
| `h` / `q` / `Esc` | Go back; `q` exits from the library list |
| `?` | Show contextual help |
| `Ctrl-C` | Exit and restore the terminal |

Articles are parsed by the `html2text` rich HTML5 engine. Headings, emphasis, code, tables, links, and images receive distinct terminal styles. Internal Wiki links open inside the TUI and keep article history. External links use the system opener.

Authenticated images are downloaded to a session-scoped temporary directory before the system image application is launched. Small Wiki formula SVGs retain their vector paths and `viewBox`, but are written with a `1400x700` display canvas for readability. Temporary files are removed when the application exits.

### Command-line mode

The non-interactive commands are suitable for scripts and pipes.

List libraries and obtain their catalog UUID and content name:

```bash
kiwix-cli books
```

Open a library home page:

```bash
kiwix-cli home --content wikivoyage_en_all_maxi_2026-06
```

Search with the catalog UUID printed by `books`:

```bash
kiwix-cli search \
  --book 12345678-1234-5678-1234-567812345678 \
  "Rust ownership"
```

Read a locator printed by `search`:

```bash
kiwix-cli read /content/rust_docs/A/Ownership
```

Choose a random article:

```bash
kiwix-cli random --content wikivoyage_en_all_maxi_2026-06
```

Use `--width` with `read`, `home`, or `random` to override terminal-width rendering:

```bash
kiwix-cli read /content/rust_docs/A/Ownership --width 60
```

Global options may appear before or after the subcommand:

```text
--server <URL>       Kiwix server URL, or KIWIX_URL; default public Browse service
--username <NAME>    Basic Auth username, or KIWIX_USERNAME
--timeout <SECONDS>  Request timeout, default 30, range 1-300
```

Run `kiwix-cli --help` or `kiwix-cli <command> --help` for the complete interface.

## Development

### Technology

- Rust 2024 edition, minimum supported Rust version 1.88
- Ratatui and Crossterm for terminal rendering and events
- Reqwest with Rustls for blocking HTTPS requests executed on worker threads
- `html2text` rich rendering for structured Wiki HTML
- `quick-xml` for bounded OPDS/RSS parsing and SVG resizing
- Clap for command-line parsing

### Source layout

| Path | Responsibility |
| --- | --- |
| `src/main.rs` | CLI definition and command dispatch |
| `src/tui.rs` | TUI state machine, rendering, input, history, and background workers |
| `src/client.rs` | Authenticated Kiwix HTTP client, URL validation, and asset retrieval |
| `src/xml.rs` | OPDS catalog and RSS search parsers |
| `src/article.rs` | Rich article spans, styles, links, images, fragments, and hit testing |
| `src/model.rs` | Library and search result data types |
| `tests/cli.rs` | Process-level HTTP and CLI integration tests |

### Kiwix protocol mapping

The client uses the following Kiwix endpoints:

| Operation | Request |
| --- | --- |
| Library catalog | `/catalog/v2/entries?count=-1` |
| Library home | `/content/{CONTENT}` followed by a validated same-library redirect |
| Search | `/search?books.id={UUID}&pattern=...&start=...&pageLength=...&format=xml` |
| Random article | `/random?content={CONTENT}` followed by a validated same-library redirect |
| Article or asset | `/raw/{CONTENT}/content/{PATH}` |

Search and article paths retain a configured reverse-proxy base path. Catalog discovery uses the Kiwix root catalog endpoint.

### Security boundaries

- Credentials are never embedded in URLs or persisted by the application.
- Automatic HTTP redirects are disabled.
- Home and random redirect targets must remain on the configured origin and inside the requested content library.
- Article links must remain on the configured origin to be treated as internal.
- `javascript:`, `data:`, `file:`, traversal paths, malformed escapes, and control characters are rejected.
- XML DTDs and custom entities are rejected.
- Text responses are limited to 8 MiB and images to 32 MiB.
- Authenticated images are downloaded locally before an external application is launched.

### Build and verify

Run the complete local quality gate:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release
git diff --check
```

The automated test suite uses local mock HTTP servers and does not require a live Kiwix service.

The packaging script targets `x86_64-unknown-linux-gnu` by default. Set `TARGET` to another installed Linux Rust target when needed.

The Linux release archive includes the manual page at `man/man1/kiwix-cli.1`.

For manual testing against a real service:

```bash
KIWIX_URL="https://wiki.example.com" \
KIWIX_USERNAME="wiki" \
KIWIX_PASSWORD="..." \
cargo run --release
```

Keep repository verification and real-service acceptance distinct: green tests establish local behavior, while endpoint compatibility, external application launch, and terminal appearance should also be checked against the target environment.

## License

MIT. See [LICENSE](LICENSE).
