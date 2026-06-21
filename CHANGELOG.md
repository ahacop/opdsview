# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-21

### Added

- Initial release
- Terminal UI for browsing OPDS 1.2 e-book/comic catalogs
- Manage feeds from the UI (add, edit, delete), with optional HTTP Basic Auth
- Browse navigation and acquisition feeds with paging and publication detail pages
- OpenSearch full-text catalog search
- Inline cover art via Kitty/Sixel/iTerm2 graphics, falling back to Unicode half-blocks
- Download books to a `Downloads/opdsview/` folder
- Built-in EPUB reader with reflowed text, inline images, table of contents,
  in-book search, and remembered reading position
- Local downloaded-book library with client-side filtering
- On-disk caching of feed XML (15-minute TTL) and cover images

[Unreleased]: https://github.com/ahacop/opdsview/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ahacop/opdsview/releases/tag/v0.1.0
