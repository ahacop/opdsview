# opdsview

A terminal UI for browsing [OPDS](https://opds.io/) catalogs (e-book/comic
feeds), written in Rust with [ratatui](https://ratatui.rs/). Add feeds — including
ones behind HTTP Basic Auth — explore them interactively, view cover art inline,
and have responses cached locally.

## Features

- **Manage feeds from the UI** — add, edit, and delete OPDS catalogs without
  touching a config file. Each feed can carry a username/password for HTTP Basic
  Auth.
- **Browse catalogs** — drill into navigation feeds, page through results, and
  inspect publications (authors, summary, available formats).
- **Inline cover images** — covers are rendered directly in the terminal using
  the best protocol your terminal supports (Kitty, Sixel, iTerm2), falling back
  to Unicode half-blocks everywhere else.
- **Local caching** — feed responses (15-minute TTL) and cover images are cached
  on disk, so re-opening a catalog is instant and offline-friendly.
- **Responsive UI** — all network I/O and image decoding happen on a background
  thread; the interface never blocks while loading.

## Running

```sh
cargo run --release
```

A `nix develop` shell with the full toolchain is provided via `flake.nix`.

## Controls

### Feed list
| Key | Action |
| --- | --- |
| `↑`/`k`, `↓`/`j` | Move selection |
| `n` | New feed |
| `e` | Edit selected feed |
| `d` | Delete selected feed (confirm with `y`) |
| `Enter`/`l` | Open feed |
| `q` | Quit |

### Feed form
| Key | Action |
| --- | --- |
| `Tab`/`↑`/`↓` | Move between fields (Name, URL, Username, Password) |
| `Enter` | Save |
| `Esc` | Cancel |

### Browser
| Key | Action |
| --- | --- |
| `↑`/`k`, `↓`/`j` | Move selection |
| `g`/`G` | Jump to top/bottom |
| `Enter`/`l`/`→` | Follow a navigation entry |
| `Backspace`/`h`/`←` | Go back (or return to the feed list) |
| `n` | Next page |
| `q`/`Esc` | Return to the feed list |

## Storage locations

Paths follow the platform conventions (via the `directories` crate):

- **Feeds** — `feeds.json` under the config directory
  (e.g. `~/.config/opdsview/` on Linux). Note: Basic Auth passwords are stored in
  plain text here.
- **Cache** — feed XML and cover images under the cache directory
  (e.g. `~/.cache/opdsview/`), keyed by a SHA-256 of the request URL.

## Notes on terminal image support

`opdsview` queries the terminal at startup to detect its graphics protocol. On
terminals that support Kitty/Sixel/iTerm2 graphics you get true-color covers; on
others, covers render as colored half-blocks. Terminals that don't respond to the
detection query fall back to half-blocks after a short timeout.

## Development

```sh
cargo test          # parser unit tests
cargo clippy        # lints
cargo run --example parse_feed -- <opds-url>   # fetch + parse a live feed
```

A good public feed to try: `https://www.gutenberg.org/ebooks.opds/`.
