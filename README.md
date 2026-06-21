# opdsview

A terminal UI for browsing [OPDS](https://opds.io/) catalogs (e-book/comic
feeds), written in Rust with [ratatui](https://ratatui.rs/). Add feeds — including
ones behind HTTP Basic Auth — explore them interactively, view cover art inline,
and have responses cached locally.

![opdsview browsing a catalog with an inline cover and publication detail](docs/screenshot.png)

## Features

- **Manage feeds from the UI** — add, edit, and delete OPDS catalogs without
  touching a config file. Each feed can carry a username/password for HTTP Basic
  Auth.
- **Browse catalogs** — drill into navigation feeds, page through results, and
  inspect publications (authors, publication date, language, publisher, subjects,
  summary, and available formats).
- **Search** — for catalogs that advertise OpenSearch (e.g. Standard Ebooks),
  press `/` to run a full-text query and browse the results like any other feed.
- **Publication detail & downloads** — press `Enter` on a book to open a full
  detail page with its cover, full description, metadata, and a list of
  downloadable formats (with file sizes). Pick a format and download it to your
  `Downloads/opdsview/` folder.
- **Built-in EPUB reader** — read downloaded EPUBs without leaving the terminal:
  styled, reflowed text with inline images, chapter and table-of-contents
  navigation, and your reading position remembered per book. Other formats
  (PDF/AZW3/CBZ) open in your OS reader.
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
| `Enter`/`l`/`→` | Follow a navigation entry, or open a publication's detail page |
| `Backspace`/`h`/`←` | Go back (or return to the feed list) |
| `/` | Search the catalog (when supported); `Enter` runs it, `Esc` cancels |
| `n` | Next page |
| `q`/`Esc` | Return to the feed list |

### Publication detail
| Key | Action |
| --- | --- |
| `↑`/`k`, `↓`/`j` | Move between formats |
| `Enter`/`o` | Catalog: download the selected format. Library: open it in the built-in reader (EPUB), or your OS reader otherwise |
| `d` | Download the selected format (catalog) |
| `x` | Open in the external OS reader (library) |
| `Backspace`/`h`/`Esc`/`q` | Close the detail page |

### Reader (built-in EPUB viewer)
| Key | Action |
| --- | --- |
| `↑`/`k`, `↓`/`j` | Scroll |
| `Space`/`PgDn`, `PgUp` | Page down/up |
| `g`/`G` | Jump to chapter start/end |
| `n`/`l`/`→`, `p`/`h`/`←` | Next / previous chapter |
| `t` | Toggle the table of contents (`↑↓` move, `Enter` jumps, `t`/`Esc` closes) |
| `q`/`Esc`/`Backspace` | Close the reader (saving your position) |

## Storage locations

Paths follow the platform conventions (via the `directories` crate):

- **Feeds** — `feeds.json` under the config directory
  (e.g. `~/.config/opdsview/` on Linux). Note: Basic Auth passwords are stored in
  plain text here.
- **Cache** — feed XML and cover images under the cache directory
  (e.g. `~/.cache/opdsview/`), keyed by a SHA-256 of the request URL.
- **Downloads** — books are saved to an `opdsview/` subfolder of your
  `Downloads` directory (e.g. `~/Downloads/opdsview/`), falling back to the
  app data directory when no `Downloads` folder exists.

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
