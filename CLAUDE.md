# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Build
cargo build

# Build release
cargo build --release

# Run (development)
cargo run

# Run tests
cargo test

# Run a single test
cargo test <test_name>

# Check without building
cargo check

# Lint
cargo clippy

# Build and run via Docker
docker compose up --build
```

## Architecture

A Rust/Axum application that periodically converts EPUBs to HTML and serves them as a browseable directory tree. Two async tasks run concurrently via `tokio::select!` in `main.rs` — if either task exits, the process exits:

1. **Extractor** (`src/extractor.rs`): Walks `EPUBS_DIR` recursively every `POLL_INTERVAL_SECS` (60s), converts new/changed EPUBs to HTML, writes output into `HTML_DIR`. Runs immediately on startup, then sleeps between passes.

2. **Server** (`src/server.rs`): Custom Axum HTTP server on port 6969. Handles directory listing and file serving manually (tower-http `ServeDir` has no directory listing support).

### Module structure

- `src/main.rs` — entry point, spawns both tasks, binds the listener
- `src/extractor.rs` — EPUB-to-HTML conversion loop; owns `EPUBS_DIR` and `HTML_DIR` constants
- `src/server.rs` — HTTP request handler: directory listing, index.html serving, file serving
- `src/util.rs` — shared `escape_html` helper

## Deployment

Runs inside Docker. Two volumes and one exposed port:

- `/epubs` (volume) — source EPUB files, may be arbitrarily nested
- `/html` (volume) — generated HTML output, served by the web server
- `6969` (port) — HTTP

`EPUBS_DIR` and `HTML_DIR` in `src/extractor.rs` are currently set to `./epubs` and `./html` for local testing. Change to `/epubs` and `/html` for Docker.

## Extractor behavior

**Directory mirroring**: The hierarchy under `EPUBS_DIR` is mirrored into `HTML_DIR`, with each EPUB becoming a directory:
```
/epubs/sci-fi/dune.epub  →  /html/sci-fi/dune/
```

**Change detection**: Each book directory contains a `.hash` file (SHA-256 of the source EPUB). Conversion is skipped when the hash matches. On re-conversion, old `chapter_*.html` and `index.html` are deleted first so stale files from a prior version don't linger.

**Per-book output**:
- `index.html` — chapter list in EPUB spine order, linked by chapter title
- `chapter_NNN.html` — one file per HTML spine item

**EPUB parsing** (`epub` crate v2):
- `doc.spine` — `Vec<SpineItem>`; use `.idref` to get the resource ID
- `doc.resources` — `HashMap<String, ResourceItem>`; use `.path` and `.mime`
- `doc.toc` — `Vec<NavPoint>`; use `.label` and `.content` (path with optional `#fragment`)
- Title resolution priority: TOC label → `<title>` tag in chapter HTML → `"Chapter N"` fallback
- Logs a warning when 0 TOC matches are found for a book

## Server behavior

Request handling in `src/server.rs`:

1. If `path/index.html` exists → serve it directly (book index pages)
2. If path is a directory → render Apache-style listing (plain `<pre>` with `<a>` links)
3. If path is a file → serve with content-type inferred from extension
4. Otherwise → 404

**Directory listing**: Directories listed first (alphabetically), then files. Dotfiles (`.hash`) are hidden. Parent `../` link shown except at root.

**Path traversal protection**: Any path component equal to `..` (`Component::ParentDir`) returns 403.
