//! Directory listing extension.
//!
//! When the inner service returns a 404 and the request path maps to a
//! directory, this middleware generates an HTML listing of the directory
//! contents. Entries are sorted, with a `..` link for navigation.

use crate::server;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use std::ffi::OsStr;
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// Middleware that provides directory listings as a fallback for 404 responses.
///
/// Also replaces bare 404 responses with HTML error pages.
pub async fn dir_list_middleware(
    State(root_dir): State<PathBuf>,
    req: Request,
    next: Next,
) -> Response {
    let uri_path = req.uri().path().to_string();
    let resp = next.run(req).await;

    if resp.status() != StatusCode::NOT_FOUND {
        return resp;
    }

    // Try to list the directory.
    match try_list_dir(&uri_path, &root_dir).await {
        Ok(Some(listing)) => listing,
        Ok(None) => server::error_response(StatusCode::NOT_FOUND),
        Err(e) => e.into_response(),
    }
}

async fn try_list_dir(
    uri_path: &str,
    root_dir: &Path,
) -> Result<Option<Response>, server::Error> {
    let path = server::local_path_for_request(uri_path, root_dir)?;

    let meta = match tokio::fs::metadata(&path).await {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };

    if !meta.is_dir() {
        return Ok(None);
    }

    tracing::trace!("directory listing: {}", path.display());

    let html = build_listing(root_dir, &path).await?;
    Ok(Some(server::html_response(html, StatusCode::OK)))
}

async fn build_listing(root_dir: &Path, dir_path: &Path) -> Result<String, server::Error> {
    let mut entries = tokio::fs::read_dir(dir_path).await?;
    let mut paths: Vec<PathBuf> = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
        paths.push(entry.path());
    }

    paths.sort();

    // Prepend ".." for parent navigation.
    let up_dir = dir_path.join("..");
    let all_paths: Vec<PathBuf> = std::iter::once(up_dir).chain(paths).collect();

    let body = format_listing(root_dir, &all_paths);
    Ok(server::render_html("", &body))
}

fn format_listing(root_dir: &Path, paths: &[PathBuf]) -> String {
    // Percent-encode set for URLs.
    // https://url.spec.whatwg.org/#fragment-percent-encode-set
    const FRAGMENT_SET: &AsciiSet = &CONTROLS.add(b' ').add(b'"').add(b'<').add(b'>').add(b'`');
    const PATH_SET: &AsciiSet = &FRAGMENT_SET.add(b'#').add(b'?').add(b'{').add(b'}');

    let dot_dot = OsStr::new("..");
    let mut buf = String::new();
    writeln!(buf, "<div>").unwrap();

    for path in paths {
        let full_url = match path.strip_prefix(root_dir) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("strip prefix error: {}", e);
                continue;
            }
        };

        let maybe_dot_dot = || {
            if path.ends_with("..") {
                Some(dot_dot)
            } else {
                None
            }
        };

        let Some(file_name) = path.file_name().or_else(maybe_dot_dot) else {
            tracing::warn!("path without file name: {}", path.display());
            continue;
        };

        let Some(file_name) = file_name.to_str() else {
            tracing::warn!("non-unicode path: {}", file_name.to_string_lossy());
            continue;
        };

        let Some(full_url_str) = full_url.to_str() else {
            tracing::warn!("non-unicode url: {}", full_url.to_string_lossy());
            continue;
        };

        let encoded_url = utf8_percent_encode(full_url_str, PATH_SET);
        writeln!(buf, "<div><a href='/{}'>{}</a></div>", encoded_url, file_name).unwrap();
    }

    writeln!(buf, "</div>").unwrap();
    buf
}
