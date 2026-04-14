use std::path::{Component, Path, PathBuf};

use percent_encoding::percent_decode_str;
use axum::{
    Json, Router,
    extract::Query,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::extractor::extractor::HTML_DIR;
use crate::extractor::read_position::{ReadPosition, ReadPositionFileData};
use crate::server::endpoints;

pub fn router() -> Router {
    Router::new()
        .route(endpoints::UPDATE_READ_POSITION, post(update_read_position))
        .route(endpoints::GET_READ_POSITION, get(get_read_position))
}

/// Parse a URL path like "/sci-fi/dune/section_001.html" into
/// (path to .info.json, section stem "section_001").
/// Returns None for invalid paths (traversal, not a section file, no parent dir).
fn parse_section_path(url_path: &str) -> Option<(PathBuf, String)> {
    let decoded = percent_decode_str(url_path)
        .decode_utf8()
        .ok()?;
    let stripped = decoded.trim_start_matches('/');
    let rel = Path::new(stripped);
    if rel.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let stem = rel.file_stem()?.to_str()?.to_string();
    if !stem.starts_with("section_") {
        return None;
    }
    let parent = rel.parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    Some((PathBuf::from(HTML_DIR).join(parent).join(".info.json"), stem))
}

#[derive(Deserialize)]
pub struct UpdateReadPositionRequest {
    pub path: String,
    pub node_path: Vec<usize>,
    pub offset: usize,
}

#[derive(Serialize)]
pub struct UpdateReadPositionResponse {
    pub success: bool,
}

async fn update_read_position(
    Json(payload): Json<UpdateReadPositionRequest>,
) -> impl IntoResponse {
    let Some((info_json_path, section_stem)) = parse_section_path(&payload.path) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(UpdateReadPositionResponse { success: false }),
        );
    };

    let bytes = match fs::read(&info_json_path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "update_read_position: .info.json not found at {}",
                info_json_path.display()
            );
            return (
                StatusCode::NOT_FOUND,
                Json(UpdateReadPositionResponse { success: false }),
            );
        }
        Err(e) => {
            eprintln!("update_read_position: read error: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(UpdateReadPositionResponse { success: false }),
            );
        }
    };

    let mut data: ReadPositionFileData = match serde_json::from_slice(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("update_read_position: parse error: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(UpdateReadPositionResponse { success: false }),
            );
        }
    };

    data.read_position.insert(
        section_stem.clone(),
        ReadPosition {
            file_name: section_stem,
            node_path: payload.node_path,
            offset: payload.offset,
        },
    );

    match serde_json::to_vec_pretty(&data) {
        Ok(json) => {
            if let Err(e) = fs::write(&info_json_path, json).await {
                eprintln!("update_read_position: write error: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(UpdateReadPositionResponse { success: false }),
                );
            }
        }
        Err(e) => {
            eprintln!("update_read_position: serialize error: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(UpdateReadPositionResponse { success: false }),
            );
        }
    }

    (StatusCode::OK, Json(UpdateReadPositionResponse { success: true }))
}

#[derive(Deserialize)]
pub struct GetReadPositionQuery {
    pub path: String,
}

#[derive(Serialize)]
pub struct GetReadPositionResponse {
    pub node_path: Vec<usize>,
    pub offset: usize,
}

async fn get_read_position(Query(params): Query<GetReadPositionQuery>) -> impl IntoResponse {
    let default_response = Json(GetReadPositionResponse {
        node_path: vec![],
        offset: 0,
    });

    let Some((info_json_path, section_stem)) = parse_section_path(&params.path) else {
        return (StatusCode::OK, default_response);
    };

    let bytes = match fs::read(&info_json_path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (StatusCode::OK, default_response);
        }
        Err(e) => {
            eprintln!("get_read_position: read error: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(GetReadPositionResponse {
                    node_path: vec![],
                    offset: 0,
                }),
            );
        }
    };

    let data: ReadPositionFileData = match serde_json::from_slice(&bytes) {
        Ok(d) => d,
        Err(_) => return (StatusCode::OK, default_response),
    };

    match data.read_position.get(&section_stem) {
        Some(pos) => (
            StatusCode::OK,
            Json(GetReadPositionResponse {
                node_path: pos.node_path.clone(),
                offset: pos.offset,
            }),
        ),
        None => (StatusCode::OK, default_response),
    }
}
