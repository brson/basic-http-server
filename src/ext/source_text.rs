//! Source code text extension.
//!
//! Overrides the Content-Type of known source code and metadata files to
//! `text/plain`, so they render directly in the browser instead of being
//! downloaded.

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::path::PathBuf;

use crate::server;

/// File extensions that should be served as plain text.
#[rustfmt::skip]
static TEXT_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "csv", "fst", "h",
    "java", "md", "mk", "proto", "py",
    "rb", "rs", "rst", "sh", "toml", "yml",
];

/// Named files that should be served as plain text.
#[rustfmt::skip]
static TEXT_FILES: &[&str] = &[
    ".gitattributes", ".gitignore", ".mailmap",
    "AUTHORS", "CODE_OF_CONDUCT", "CONTRIBUTING",
    "COPYING", "COPYRIGHT", "Cargo.lock",
    "LICENSE", "LICENSE-APACHE", "LICENSE-MIT",
    "Makefile", "rust-toolchain",
];

/// Middleware that overrides Content-Type to text/plain for source code files.
pub async fn source_text_middleware(
    State(root_dir): State<PathBuf>,
    req: Request,
    next: Next,
) -> Response {
    let uri_path = req.uri().path().to_string();
    let resp = next.run(req).await;

    if !should_convert(&uri_path) {
        return resp;
    }

    // Check if it's a directory listing (already HTML from dir_list middleware).
    // Don't override directories that happened to match.
    if let Ok(path) = server::local_path_for_request(&uri_path, &root_dir) {
        if path.is_dir() {
            return resp;
        }
    }

    tracing::trace!("source text override: {}", uri_path);

    let (mut parts, body) = resp.into_parts();
    parts
        .headers
        .insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
    Response::from_parts(parts, body).into_response()
}

fn should_convert(path: &str) -> bool {
    let file_name = match path.rsplit('/').next() {
        Some(n) => n,
        None => return false,
    };

    if TEXT_FILES.contains(&file_name) {
        return true;
    }

    if let Some(ext) = file_name.rsplit('.').next() {
        if TEXT_EXTENSIONS.contains(&ext) {
            return true;
        }
    }

    false
}
