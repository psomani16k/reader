use axum::{
    Router,
    extract::{Path, Query},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use std::collections::HashMap;
use std::path::{Component, PathBuf};
use tokio::fs;

use crate::server::api_server;
use crate::server::templates::{self, BookIndex, Breadcrumb, EntryInfo};
use crate::{extractor::extractor::HTML_DIR, server::previous_path::PreviousPage};

pub fn router() -> Router {
    Router::new()
        .nest("/api", api_server::router())
        .route("/static/{*path}", get(serve_static))
        .route("/previous", get(serve_previous))
        .route("/", get(serve_path_root))
        .route("/{*path}", get(serve_path))
}

async fn serve_previous() -> Redirect {
    match PreviousPage::get().await {
        Some(previous) => Redirect::to(&previous),
        None => Redirect::to("/"),
    }
}

async fn serve_static(Path(path): Path<String>) -> Response {
    match path.as_str() {
        "common.css" => {
            let css = include_str!("assets/common.css");
            (
                [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
                css,
            )
                .into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve_path_root() -> Response {
    serve_path_impl("", false).await
}

async fn serve_path(
    Path(path): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let raw = params.contains_key("raw");
    serve_path_impl(&path, raw).await
}

async fn serve_path_impl(rel_path: &str, raw: bool) -> Response {
    // Path traversal protection: reject any component that is ".."
    let rel = std::path::Path::new(rel_path);
    if rel.components().any(|c| matches!(c, Component::ParentDir)) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let full_path = PathBuf::from(HTML_DIR).join(rel_path);

    // Try index.json first (book index), then directory listing, then file
    let index_json_path = full_path.join("index.json");
    if let Ok(bytes) = fs::read(&index_json_path).await {
        return render_book_index(&bytes, rel_path).await;
    }
    if let Ok(entries) = fs::read_dir(&full_path).await {
        return render_directory(entries, rel_path, &full_path).await;
    }
    match fs::read(&full_path).await {
        Ok(bytes) => {
            // If this is a section file inside a book and not a raw request,
            // render the section view wrapper instead of serving the raw file
            if !raw {
                if let Some(resp) = try_render_section_view(rel_path, &full_path).await {
                    // it's okay if we weren't able to store the latest path, TODO: log this
                    let _p = PreviousPage::set(&rel_path).await;
                    return resp;
                }
            }
            serve_file(&full_path, bytes)
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn render_book_index(json_bytes: &[u8], rel_path: &str) -> Response {
    let book_index: BookIndex = match serde_json::from_slice(json_bytes) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("Failed to parse index.json: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let breadcrumbs = build_breadcrumbs(rel_path);

    match templates::render_book_view(&book_index, breadcrumbs) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            eprintln!("Template render error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// If the file is a `section_*.html` inside a book directory (has sibling index.json),
/// render the section view wrapper with prev/next navigation.
async fn try_render_section_view(rel_path: &str, full_path: &std::path::Path) -> Option<Response> {
    let file_name = full_path.file_name()?.to_str()?;
    if !file_name.starts_with("section_") || !file_name.ends_with(".html") {
        return None;
    }

    let parent = full_path.parent()?;
    let index_json_path = parent.join("index.json");
    let json_bytes = fs::read(&index_json_path).await.ok()?;
    let book_index: BookIndex = serde_json::from_slice(&json_bytes).ok()?;

    // Find current section index
    let current_idx = book_index
        .sections
        .iter()
        .position(|s| s.filename == file_name)?;

    let prev_url = if current_idx > 0 {
        Some(book_index.sections[current_idx - 1].filename.clone())
    } else {
        None
    };
    let next_url = if current_idx + 1 < book_index.sections.len() {
        Some(book_index.sections[current_idx + 1].filename.clone())
    } else {
        None
    };

    // Build breadcrumbs from the parent path (the book directory)
    let parent_rel = std::path::Path::new(rel_path)
        .parent()
        .unwrap_or(std::path::Path::new(""));
    let breadcrumbs = build_breadcrumbs(parent_rel.to_str().unwrap_or(""));

    let iframe_src = format!("{file_name}?raw=true");

    match templates::render_section_view(
        &book_index.book_name,
        breadcrumbs,
        &iframe_src,
        prev_url.as_deref(),
        next_url.as_deref(),
    ) {
        Ok(html) => Some(Html(html).into_response()),
        Err(e) => {
            eprintln!("Section view render error: {e}");
            None // Fall back to raw file serving
        }
    }
}

async fn render_directory(
    mut entries: fs::ReadDir,
    rel_path: &str,
    full_path: &std::path::Path,
) -> Response {
    let mut items: Vec<(String, bool)> = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
            continue;
        };
        // Skip dotfiles (e.g. .hash)
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        let is_dir = meta.is_dir();
        items.push((name, is_dir));
    }

    // Sort: directories first, then alphabetically
    items.sort_by(|a, b| match (a.1, b.1) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.0.cmp(&b.0),
    });

    let breadcrumbs = build_breadcrumbs(rel_path);

    let current_path = if rel_path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}/", rel_path)
    };

    let parent_url = if rel_path.is_empty() {
        None
    } else {
        Some("../".to_string())
    };

    let entry_infos: Vec<EntryInfo> = items
        .into_iter()
        .map(|(name, is_dir)| {
            let is_book = is_dir && full_path.join(&name).join("index.json").exists();
            let url = if is_dir {
                format!("{name}/")
            } else {
                name.clone()
            };
            EntryInfo {
                url,
                is_dir,
                is_book,
                name,
            }
        })
        .collect();

    match templates::render_directory_view(&current_path, breadcrumbs, parent_url, entry_infos) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            eprintln!("Template render error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn build_breadcrumbs(rel_path: &str) -> Vec<Breadcrumb> {
    let mut breadcrumbs = vec![Breadcrumb {
        url: "/".to_string(),
        name: "root".to_string(),
    }];
    if !rel_path.is_empty() {
        let mut accumulated = String::new();
        for segment in rel_path.split('/') {
            if segment.is_empty() {
                continue;
            }
            accumulated.push('/');
            accumulated.push_str(segment);
            accumulated.push('/');
            breadcrumbs.push(Breadcrumb {
                url: accumulated.clone(),
                name: segment.to_string(),
            });
        }
    }
    breadcrumbs
}

fn serve_file(file_path: &std::path::Path, bytes: Vec<u8>) -> Response {
    let content_type = guess_content_type(file_path);
    ([(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response()
}

fn guess_content_type(path: &std::path::Path) -> &'static str {
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(|ext| match ext {
            "html" | "htm" => Some("text/html; charset=utf-8"),
            "css" => Some("text/css; charset=utf-8"),
            "js" => Some("application/javascript"),
            "json" => Some("application/json"),
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "gif" => Some("image/gif"),
            "svg" => Some("image/svg+xml"),
            "txt" => Some("text/plain; charset=utf-8"),
            "pdf" => Some("application/pdf"),
            "woff" => Some("font/woff"),
            "woff2" => Some("font/woff2"),
            _ => None,
        })
        .unwrap_or("application/octet-stream")
}
