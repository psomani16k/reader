use axum::{
    Router,
    extract::Path,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use std::path::{Component, PathBuf};
use tokio::fs;

use crate::extractor::HTML_DIR;
use crate::util::escape_html;

pub fn router() -> Router {
    Router::new()
        .route("/", get(serve_path_root))
        .route("/{*path}", get(serve_path))
}

async fn serve_path_root() -> Response {
    serve_path_impl("").await
}

async fn serve_path(Path(path): Path<String>) -> Response {
    serve_path_impl(&path).await
}

async fn serve_path_impl(rel_path: &str) -> Response {
    // Path traversal protection: reject any component that is ".."
    let rel = std::path::Path::new(rel_path);
    if rel.components().any(|c| matches!(c, Component::ParentDir)) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let full_path = PathBuf::from(HTML_DIR).join(rel_path);

    // Try as directory/index.html first, then as a directory view
    // then as file — avoids TOCTOU by branching on
    // the actual operation result rather than a separate metadata check.
    let index_path = full_path.join("index.html");
    if index_path.exists() {
        if let Ok(bytes) = fs::read(&index_path).await {
            return serve_file_bytes(&index_path, bytes);
        }

    }
    if let Ok(entries) = fs::read_dir(&full_path).await {
        return render_directory(entries, rel_path).await;
    }
    match fs::read(&full_path).await {
        Ok(bytes) => serve_file_bytes(&full_path, bytes),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn render_directory(mut entries: fs::ReadDir, rel_path: &str) -> Response {
    let mut items = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
            continue;
        };
        // Skip dotfiles (e.g. .hash)
        if name.starts_with('.') {
            continue;
        }
        let is_dir = entry.metadata().await.map_or(false, |m| m.is_dir());
        items.push((name, is_dir));
    }

    // Sort: directories first, then alphabetically
    items.sort_by(|a, b| match (a.1, b.1) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.0.cmp(&b.0),
    });

    let page_title = if rel_path.is_empty() {
        "Index"
    } else {
        rel_path.split('/').last().unwrap_or("")
    };

    let mut html = format!(
        "<!DOCTYPE html>\n<html>\n<head><meta charset=\"utf-8\"><title>{}</title></head>\n<body>\n<pre>\n",
        escape_html(page_title)
    );

    // Parent link (unless at root)
    if !rel_path.is_empty() {
        html.push_str("<a href=\"../\">../</a>\n");
    }

    for (name, is_dir) in items {
        let display = if is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        html.push_str(&format!(
            "<a href=\"{}\">{}</a>\n",
            escape_html(&display),
            escape_html(&display)
        ));
    }

    html.push_str("</pre>\n</body>\n</html>\n");
    Html(html).into_response()
}

fn serve_file_bytes(file_path: &std::path::Path, bytes: Vec<u8>) -> Response {
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
