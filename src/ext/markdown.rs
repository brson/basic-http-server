//! Markdown rendering extension.
//!
//! Intercepts requests for `.md` files, renders them to HTML using comrak
//! with GitHub-flavored Markdown options and syntect syntax highlighting,
//! and returns the rendered page.

use crate::server;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use comrak::plugins::syntect::SyntectAdapter;
use comrak::{markdown_to_html_with_plugins, Options, Plugins};
use std::path::PathBuf;
use std::sync::LazyLock;
use syntect::highlighting::ThemeSet;
use syntect::html::{css_for_theme_with_class_style, ClassStyle};

/// CSS for syntax highlighting, generated once from syntect themes.
///
/// Light theme is the default; dark theme is wrapped in a
/// `prefers-color-scheme: dark` media query.
static SYNTAX_CSS: LazyLock<String> = LazyLock::new(|| {
    let ts = ThemeSet::load_defaults();
    let style = ClassStyle::Spaced;
    let light = css_for_theme_with_class_style(&ts.themes["InspiredGitHub"], style)
        .expect("light theme CSS");
    let dark = css_for_theme_with_class_style(&ts.themes["base16-ocean.dark"], style)
        .expect("dark theme CSS");
    format!("{light}\n@media (prefers-color-scheme: dark) {{\n{dark}\n}}")
});

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

    // Enable syntax highlighting with CSS classes.
    let adapter = SyntectAdapter::new(None);
    let mut plugins = Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&adapter);

    let html_body = markdown_to_html_with_plugins(&source, &options, &plugins);

    // Prepend syntax theme CSS to the body.
    let body_with_css = format!("<style>\n{}\n</style>\n{}", &*SYNTAX_CSS, html_body);
    let html = server::render_html("", &body_with_css);

    Ok(server::html_response(html, StatusCode::OK))
}
