//! Core server utilities: error types, HTML rendering, error responses.

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use std::io;

/// The HTML template, rendered with `format!`.
static HTML_TEMPLATE: &str = include_str!("template.html");

/// Render an HTML page with a title and body.
pub fn render_html(title: &str, body: &str) -> String {
    HTML_TEMPLATE
        .replace("{title}", title)
        .replace("{body}", body)
}

/// Render an error page from an HTTP status code.
pub fn error_response(status: StatusCode) -> Response {
    error_response_with_headers(status, HeaderMap::new())
}

/// Render an error page with additional headers.
pub fn error_response_with_headers(status: StatusCode, extra_headers: HeaderMap) -> Response {
    let html = render_html(&status.to_string(), "");
    let mut response = (status, [(header::CONTENT_TYPE, "text/html")], html).into_response();
    response.headers_mut().extend(extra_headers);
    response
}

/// The server error type.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error")]
    Io(#[from] io::Error),

    #[error("HTTP error")]
    Http(#[from] http::Error),

    #[error("requested URI is not UTF-8")]
    UriNotUtf8,

    #[error("requested URI is not an absolute path")]
    UriNotAbsolute,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match &self {
            Error::Io(e) if e.kind() == io::ErrorKind::NotFound => {
                tracing::debug!("{}", e);
                error_response(StatusCode::NOT_FOUND)
            }
            Error::Io(e) => {
                tracing::error!("I/O error: {}", e);
                error_response(StatusCode::INTERNAL_SERVER_ERROR)
            }
            e => {
                tracing::error!("internal error: {}", e);
                error_response(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }
}

/// Map a request URI to a local filesystem path.
pub fn local_path_for_request(
    uri_path: &str,
    root_dir: &std::path::Path,
) -> Result<std::path::PathBuf, Error> {
    use percent_encoding::percent_decode_str;

    tracing::debug!("raw URI path: {}", uri_path);

    // Trim off query parameters.
    let end = uri_path.find('?').unwrap_or(uri_path.len());
    let request_path = &uri_path[..end];

    // Decode percent-encoding.
    let decoded = percent_decode_str(request_path)
        .decode_utf8()
        .map_err(|_| {
            tracing::error!("non-UTF-8 URL: {}", request_path);
            Error::UriNotUtf8
        })?;

    // Build the local path.
    let mut path = root_dir.to_owned();
    if let Some(rest) = decoded.strip_prefix('/') {
        path.push(rest);
    } else {
        tracing::warn!("non-absolute path: {}", decoded);
        return Err(Error::UriNotAbsolute);
    }

    tracing::debug!("resolved path: {}", path.display());
    Ok(path)
}

/// Make an HTTP response from an HTML string.
pub fn html_response(body: String, status: StatusCode) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(body.len()));
    (status, headers, body).into_response()
}
