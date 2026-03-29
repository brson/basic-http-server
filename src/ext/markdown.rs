//! Markdown rendering extension.
//!
//! Intercepts requests for `.md` files, renders them to HTML using comrak
//! with GitHub-flavored Markdown options, and returns the rendered page.

use crate::server;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use comrak::{markdown_to_html, Options};
use std::path::PathBuf;

/// Middleware that renders `.md` files to HTML.
pub async fn markdown_middleware(
    State(root_dir): State<PathBuf>,
    req: Request,
    next: Next,
) -> Response {
    let uri_path = req.uri().path().to_string();

    if !uri_path.ends_with(".md") {
        return next.run(req).await;
    }

    tracing::trace!("markdown extension: {}", uri_path);

    match render_markdown(&uri_path, &root_dir).await {
        Ok(resp) => resp,
        Err(e) => e.into_response(),
    }
}

async fn render_markdown(uri_path: &str, root_dir: &PathBuf) -> Result<Response, server::Error> {
    let path = server::local_path_for_request(uri_path, root_dir)?;

    let buf = tokio::fs::read(&path).await?;
    let source = String::from_utf8(buf).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "markdown is not UTF-8")
    })?;

    // Render with GitHub-flavored Markdown options.
    let mut options = Options::default();
    options.extension.autolink = true;
    options.extension.header_ids = Some("user-content-".to_string());
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tagfilter = true;
    options.extension.tasklist = true;
    options.render.github_pre_lang = true;

    let html_body = markdown_to_html(&source, &options);
    let html = server::render_html("", &html_body);

    Ok(server::html_response(html, StatusCode::OK))
}
