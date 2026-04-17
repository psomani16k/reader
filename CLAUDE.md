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

2. **Server** (`src/server/`): Custom Axum HTTP server on port 6969. Handles directory listing, book/section views via Tera templates, file serving, and read-position API.

### Module structure

- `src/main.rs` — entry point, spawns both tasks, binds the listener
- `src/extractor.rs` — EPUB-to-HTML conversion loop; owns `EPUBS_DIR` and `HTML_DIR` constants
- `src/extractor/read_position.rs` — `ReadPosition` struct and `.info.json` (de)serialization
- `src/server/mod.rs` — server module declarations
- `src/server/server.rs` — HTTP router; directory listing, book/section view dispatch, raw file serving
- `src/server/api_server.rs` — REST endpoints for read-position tracking
- `src/server/endpoints.rs` — API path constants
- `src/server/templates.rs` — Tera template rendering helpers
- `src/server/assets/` — embedded HTML templates (`directory_view.html`, `book_view.html`, `section_view.html`) and `common.css`
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

**Change detection**: Each book directory contains a `.hash` file (SHA-256 of the source EPUB). Conversion is skipped when the hash matches. On re-conversion, old `section_*.html` and `index.json` are deleted first so stale files from a prior version don't linger.

**Per-book output**:
- `index.json` — chapter list in EPUB spine order: `{ "book_name": "...", "sections": [{ "title": "...", "filename": "section_001.html" }, ...] }`
- `section_NNN.html` — one file per HTML spine item, with scroll-tracking JavaScript injected
- `.info.json` — read-position store, created on first extraction: `{ "read_position": { "<section_stem>": ReadPosition, ... } }`

**EPUB parsing** (`epub` crate v2):
- `doc.spine` — `Vec<SpineItem>`; use `.idref` to get the resource ID
- `doc.resources` — `HashMap<String, ResourceItem>`; use `.path` and `.mime`
- `doc.toc` — `Vec<NavPoint>`; use `.label` and `.content` (path with optional `#fragment`)
- Title resolution priority: TOC label → `<title>` tag in chapter HTML → `"Chapter N"` fallback
- Logs a warning when 0 TOC matches are found for a book

**Resource path rewriting**: All `src=` and `href=` attributes in extracted HTML are rewritten to flatten the EPUB internal path structure. Relative paths (e.g. `../images/foo.jpg`) are resolved against the section's original location and rewritten to book-root-relative paths. Absolute URLs, data URIs, and fragments are left untouched.

**Scroll-tracking injection**: The extractor appends a JavaScript snippet to every `section_*.html` that:
- Finds the topmost visible DOM element via `elementFromPoint` and records its child-index path
- Debounces scroll events (500 ms) and POSTs position to `POST /api/updateReadPosition`
- On page load, fetches the saved position via `GET /api/readPosition?path=...` and calls `scrollIntoView()`

## Server behavior

Request routing priority in `src/server/server.rs`:

1. `/api/*` → `api_server` router (read-position endpoints)
2. `/static/{path}` → serve embedded static assets (currently only `common.css`)
3. `/{path}` → `serve_path_impl()`:
   a. Parent directory contains `index.json` and file matches `section_*.html` → render **section view** (unless `?raw=true`)
   b. `path/index.json` exists → render **book view** (chapter list)
   c. Path is a directory → render **directory listing**
   d. Path is a file → serve raw bytes with content-type from extension
   e. Otherwise → 404

**Section view** (`section_view.html`): wraps a `section_*.html` in an iframe with:
- Previous / Next navigation buttons derived from `index.json` spine order
- Padding slider (0–30%) that narrows the iframe symmetrically; value persisted in `localStorage` as `reader-padding`

**Book view** (`book_view.html`): renders the chapter list from `index.json` with a parent breadcrumb.

**Directory listing** (`directory_view.html`): Apache-style listing. Directories first (alphabetically), then files. Dotfiles hidden. Parent `../` link shown except at root.

**Path traversal protection**: Any path component equal to `..` (`Component::ParentDir`) returns 403.

## Read-position API

Defined in `src/server/api_server.rs`. Paths are constants in `src/server/endpoints.rs`.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/readPosition?path=<url-encoded section path>` | Returns saved `ReadPosition` JSON for that section, or `null` |
| `POST` | `/api/updateReadPosition` | Body: `{ "path": "...", "node_path": [...], "offset": N }` — writes position to `.info.json` |

**`ReadPosition`** (`src/extractor/read_position.rs`):
```rust
struct ReadPosition {
    file_name: String,       // section stem, e.g. "section_001"
    node_path: Vec<usize>,   // child-index path from <body> to topmost visible element
    offset: usize,           // pixels scrolled past that element
}
```

`.info.json` lives in the book directory alongside `index.json` and is updated in-place on every position save.
