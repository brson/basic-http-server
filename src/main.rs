//! A simple HTTP server, for learning and local development.
//!
//! This server demonstrates how to build an async HTTP file server with axum,
//! tower-http, and tokio. It serves static files from a root directory, with
//! optional developer extensions enabled by the `-x` flag.

use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use clap::Parser;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use tower_http::services::ServeDir;
use tracing::info;

mod ext;
mod server;

/// A basic HTTP file server.
#[derive(Clone, Parser)]
#[command(version, about = "A basic HTTP file server")]
pub struct Config {
    /// The IP:PORT combination.
    #[arg(short = 'a', long = "addr", conflicts_with_all = ["port", "public"])]
    addr: Option<SocketAddr>,

    /// Port number.
    #[arg(short = 'p', long = "port")]
    port: Option<u16>,

    /// Listen on all interfaces (0.0.0.0) instead of localhost.
    #[arg(long = "public")]
    public: bool,

    /// The root directory for serving files.
    #[arg(default_value = ".")]
    root_dir: PathBuf,

    /// Enable developer extensions.
    #[arg(short = 'x')]
    use_extensions: bool,
}

impl Config {
    /// Resolve the listen address from the combination of --addr, --port, and --public.
    fn listen_addr(&self) -> SocketAddr {
        if let Some(addr) = self.addr {
            return addr;
        }
        let ip = if self.public {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };
        let port = self.port.unwrap_or(4000);
        SocketAddr::new(ip, port)
    }
}

#[tokio::main]
async fn main() {
    // Initialize tracing. Default to "info" for this crate unless RUST_LOG is set.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "basic_http_server=info".parse().unwrap()),
        )
        .with_target(false)
        .without_time()
        .init();

    let config = Config::parse();
    let addr = config.listen_addr();

    info!("basic-http-server {}", env!("CARGO_PKG_VERSION"));
    info!("addr: http://{}", addr);
    info!("root dir: {}", config.root_dir.display());
    info!("extensions: {}", config.use_extensions);

    let app = build_router(&config);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind address");

    let local_addr = listener.local_addr().unwrap();
    info!("listening on {}", local_addr);
    // Also print to stderr without buffering, for integration test harness.
    eprintln!("listening on {}", local_addr);

    axum::serve(listener, app)
        .await
        .expect("server error");
}

/// Build the axum router.
///
/// The core server uses `tower_http::ServeDir` for static file serving. When
/// extensions are enabled, tower middleware layers are added for each extension
/// feature. The extensions are in the `ext` module.
fn build_router(config: &Config) -> Router {
    // ServeDir handles: static files with streaming, MIME detection,
    // Content-Length, index.html fallback, and trailing-slash redirects.
    let serve_dir = ServeDir::new(&config.root_dir)
        .append_index_html_on_directories(true);

    // When extensions are enabled, wrap ServeDir with extension middleware.
    // When disabled, the router is a clean, minimal static file server.
    if config.use_extensions {
        let config_clone = config.clone();
        Router::new()
            .fallback_service(serve_dir)
            .layer(middleware::from_fn(method_filter))
            .layer(middleware::from_fn(cache_control))
            .layer(middleware::from_fn_with_state(
                config_clone.root_dir.clone(),
                ext::source_text_middleware,
            ))
            .layer(middleware::from_fn_with_state(
                config_clone.root_dir.clone(),
                ext::dir_list_middleware,
            ))
            .layer(middleware::from_fn_with_state(
                config_clone.root_dir.clone(),
                ext::markdown_middleware,
            ))
    } else {
        Router::new()
            .fallback_service(serve_dir)
            .layer(middleware::from_fn(method_filter))
            .layer(middleware::from_fn(cache_control))
            .layer(middleware::from_fn(not_found_html))
    }
}

/// Middleware that sets `Cache-Control: no-cache` on all responses.
///
/// This ensures clients always revalidate with the server before using a
/// cached copy. They still benefit from conditional requests (304 Not
/// Modified via `Last-Modified` / `ETag`), but won't silently serve stale
/// files — important for a local development server.
async fn cache_control(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    resp
}

/// Middleware that enforces GET-only requests.
///
/// Returns 405 Method Not Allowed with an `Allow: GET` header for any
/// non-GET request.
async fn method_filter(req: Request, next: Next) -> Response {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        let mut headers = HeaderMap::new();
        headers.insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return server::error_response_with_headers(StatusCode::METHOD_NOT_ALLOWED, headers);
    }
    next.run(req).await
}

/// Middleware that replaces non-HTML 404 responses with HTML error pages.
async fn not_found_html(req: Request, next: Next) -> Response {
    let resp = next.run(req).await;
    if resp.status() == StatusCode::NOT_FOUND {
        return server::error_response(StatusCode::NOT_FOUND);
    }
    resp
}
