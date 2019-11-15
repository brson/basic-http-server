//! A simple HTTP server, for learning and local doc development.

use env_logger::{Builder, Env};
use futures::future;
use futures::FutureExt;
use handlebars::Handlebars;
use http::status::StatusCode;
use http::Uri;
use hyper::service::{make_service_fn, service_fn};
use hyper::{header, Body, Request, Response, Server};
use log::{debug, error, info, trace, warn};
use percent_encoding::percent_decode_str;
use serde::Serialize;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use structopt::StructOpt;
use thiserror::Error;
use tokio::runtime::Runtime;

// Developer extensions. These are contained in their own module so that the
// principle HTTP server behavior is not obscured.
mod ext;

fn main() {
    // Set up error handling immediately
    if let Err(e) = run() {
        log_error_chain(&e);
    }
}

/// Basic error reporting, including the "cause chain". This is used both by the
/// top-level error reporting and to report internal server errors.
fn log_error_chain(mut e: &dyn std::error::Error) {
    error!("error: {}", e);
    while let Some(source) = e.source() {
        error!("caused by: {}", source);
        e = source;
    }
}

/// The configuration object, parsed from command line options.
#[derive(Clone, StructOpt)]
#[structopt(about = "A basic HTTP file server")]
pub struct Config {
    /// The IP:PORT combination.
    #[structopt(
        name = "ADDR",
        short = "a",
        long = "addr",
        parse(try_from_str),
        default_value = "127.0.0.1:4000"
    )]
    addr: SocketAddr,

    /// The root directory for serving files.
    #[structopt(name = "ROOT", parse(from_os_str), default_value = ".")]
    root_dir: PathBuf,

    /// Enable developer extensions.
    #[structopt(short = "x")]
    use_extensions: bool,
}

fn run() -> Result<()> {
    // Initialize logging, and log the "info" level for this crate only, unless
    // the environment contains `RUST_LOG`.
    let env = Env::new().default_filter_or("basic_http_server=info");
    Builder::from_env(env)
        .format_module_path(false)
        .format_timestamp(None)
        .init();

    // Create the configuration from the command line arguments. It
    // includes the IP address and port to listen on and the path to use
    // as the HTTP server's root directory.
    let config = Config::from_args();

    // Display the configuration to be helpful
    info!("basic-http-server {}", env!("CARGO_PKG_VERSION"));
    info!("addr: http://{}", config.addr);
    info!("root dir: {}", config.root_dir.display());
    info!("extensions: {}", config.use_extensions);

    // Create the MakeService object that creates a new Hyper service for every
    // connection. Both these closures need to return a Future of Result, and we
    // use two different mechanisms to achieve that.
    let make_service = make_service_fn(|_| {
        let config = config.clone();

        let service = service_fn(move |req| {
            let config = config.clone();

            // Handle the request, returning a Future of Response,
            // and map it to a Future of Result of Response.
            serve(config, req).map(Ok::<_, Error>)
        });

        // Convert the concrete (non-future) service function to a Future of Result.
        future::ok::<_, Error>(service)
    });

    // Create a Hyper Server, binding to an address, and use
    // our service builder.
    let server = Server::bind(&config.addr).serve(make_service);

    // Create a Tokio runtime and block on Hyper forever.
    let rt = Runtime::new()?;
    rt.block_on(server)?;

    Ok(())
}

/// Create an HTTP Response future for each Request.
///
/// Errors are turned into an Error response (404 or 500), and never propagated
/// upward for hyper to deal with.
async fn serve(config: Config, req: Request<Body>) -> Response<Body> {
    // Serve the requested file.
    let resp = serve_file(&req, &config.root_dir).await;

    // Give developer extensions an opportunity to post-process the request/response pair.
    let resp = ext::serve(config, req, resp).await;

    // Transform internal errors to error responses.
    let resp = transform_error(resp).await;

    resp
}

/// Turn any errors into an HTTP error response.
async fn transform_error(resp: Result<Response<Body>>) -> Response<Body> {
    match resp {
        Ok(r) => r,
        Err(e) => {
            let resp = make_error_response(e).await;
            match resp {
                Ok(r) => r,
                Err(e) => {
                    // Last-ditch error reporting if even making the error response failed.
                    error!("unexpected internal error: {}", e);
                    Response::new(Body::from(format!("unexpected internal error: {}", e)))
                }
            }
        }
    }
}

/// Serve static files from a root directory.
async fn serve_file(req: &Request<Body>, root_dir: &PathBuf) -> Result<Response<Body>> {
    // First, try to do a redirect. If that doesn't happen, then find the path
    // to the static file we want to serve - which may be `index.html` for
    // directories - and send a response containing that file.
    let maybe_redir_resp = try_dir_redirect(req, &root_dir)?;

    if let Some(redir_resp) = maybe_redir_resp {
        return Ok(redir_resp);
    }

    if let Some(path) = local_path_with_maybe_index(req.uri(), &root_dir) {
        Ok(respond_with_file(path).await?)
    } else {
        Err(Error::UrlToPath)
    }
}

/// Try to do a 302 redirect for directories.
///
/// If we get a URL without trailing "/" that can be mapped to a directory, then
/// return a 302 redirect to the path with the trailing "/".
///
/// Without this we couldn't correctly return the contents of `index.html` for a
/// directory - for the purpose of building absolute URLs from relative URLs,
/// agents appear to only treat paths with trailing "/" as directories, so we
/// have to redirect to the proper directory URL first.
///
/// In other words, if we returned the contents of `index.html` for URL `docs`
/// then all the relative links in that file would be broken, but that is not
/// the case for URL `docs/`.
///
/// This seems to match the behavior of other static web servers.
fn try_dir_redirect(req: &Request<Body>, root_dir: &PathBuf) -> Result<Option<Response<Body>>> {
    if req.uri().path().ends_with("/") {
        return Ok(None);
    }

    debug!("path does not end with /");
    if let Some(path) = local_path_for_request(req.uri(), root_dir) {
        if !path.is_dir() {
            return Ok(None);
        }

        let mut new_loc = req.uri().path().to_string();
        new_loc.push_str("/");
        if let Some(query) = req.uri().query() {
            new_loc.push_str("?");
            new_loc.push_str(query);
        }

        info!("redirecting {} to {}", req.uri(), new_loc);
        Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, new_loc)
            .body(Body::empty())
            .map(Some)
            .map_err(Error::from)
    } else {
        Err(Error::UrlToPath)
    }
}

/// Read the file completely and construct a 200 response with that file as the
/// body of the response. If the I/O here fails then an error future will be
/// returned, and `serve` will convert it into the appropriate HTTP error
/// response.
async fn respond_with_file(path: PathBuf) -> Result<Response<Body>> {
    let buf = tokio::fs::read(&path).await?;
    let mime_type = file_path_mime(&path);
    let resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, buf.len() as u64)
        .header(header::CONTENT_TYPE, mime_type.as_ref())
        .body(Body::from(buf))?;
    Ok(resp)
}

/// Get a MIME type based on the file extension
fn file_path_mime(file_path: &Path) -> mime::Mime {
    let mime_type = match file_path.extension().and_then(std::ffi::OsStr::to_str) {
        Some("html") => mime::TEXT_HTML,
        Some("css") => mime::TEXT_CSS,
        Some("js") => mime::TEXT_JAVASCRIPT,
        Some("jpg") => mime::IMAGE_JPEG,
        Some("md") => "text/markdown; charset=UTF-8"
            .parse::<mime::Mime>()
            .unwrap(),
        Some("png") => mime::IMAGE_PNG,
        Some("svg") => mime::IMAGE_SVG,
        Some("wasm") => "application/wasm".parse::<mime::Mime>().unwrap(),
        _ => mime::TEXT_PLAIN,
    };
    mime_type
}

/// Find the local path for a request URI, converting directories to the
/// `index.html` file.
fn local_path_with_maybe_index(uri: &Uri, root_dir: &Path) -> Option<PathBuf> {
    local_path_for_request(uri, root_dir).map(|mut p: PathBuf| {
        if p.is_dir() {
            p.push("index.html");
            debug!("trying {} for directory URL", p.display());
        } else {
            trace!("trying path as from URL");
        }
        p
    })
}

/// Map the request's URI to a local path
fn local_path_for_request(uri: &Uri, root_dir: &Path) -> Option<PathBuf> {
    debug!("raw URI: {}", uri);

    let request_path = uri.path();

    debug!("raw URI to path: {}", request_path);

    // This is equivalent to checking for hyper::RequestUri::AbsoluteUri
    if !request_path.starts_with("/") {
        warn!("found non-absolute path {}", request_path);
        return None;
    }

    // Trim off the url parameters starting with '?'
    let end = request_path.find('?').unwrap_or(request_path.len());
    let request_path = &request_path[0..end];

    // Convert %-encoding to actual values
    let decoded = percent_decode_str(&request_path);
    let request_path = if let Ok(p) = decoded.decode_utf8() {
        p
    } else {
        error!("unable to percent-decode URL: {}", request_path);
        // FIXME: Error handling
        return None;
    };

    // Append the requested path to the root directory
    let mut path = root_dir.to_owned();
    if request_path.starts_with('/') {
        path.push(&request_path[1..]);
    } else {
        warn!("found non-absolute path {}", request_path);
        return None;
    }

    debug!("URL · path : {} · {}", uri, path.display());

    Some(path)
}

/// Convert an error to an HTTP error response future, with correct response code.
async fn make_error_response(e: Error) -> Result<Response<Body>> {
    let resp = match e {
        Error::Io { source: e } => make_io_error_response(e).await?,
        Error::Ext {
            source: ext::Error::Io { source: e },
        } => make_io_error_response(e).await?,
        e => make_internal_server_error_response(e).await?,
    };
    Ok(resp)
}

/// Convert an error into a 500 internal server error, and log it.
async fn make_internal_server_error_response(err: Error) -> Result<Response<Body>> {
    log_error_chain(&err);
    let resp = make_error_response_from_code(StatusCode::INTERNAL_SERVER_ERROR).await?;
    Ok(resp)
}

/// Handle the one special io error (file not found) by returning a 404, otherwise
/// return a 500.
async fn make_io_error_response(error: io::Error) -> Result<Response<Body>> {
    let resp = match error.kind() {
        io::ErrorKind::NotFound => {
            debug!("{}", error);
            make_error_response_from_code(StatusCode::NOT_FOUND).await?
        }
        _ => make_internal_server_error_response(error.into()).await?,
    };
    Ok(resp)
}

/// Make an error response given an HTTP status code.
async fn make_error_response_from_code(status: StatusCode) -> Result<Response<Body>> {
    let body = render_error_html(status)?;
    let resp = html_str_to_response(body, status)?;
    Ok(resp)
}

/// Make an HTTP response from a HTML string.
fn html_str_to_response(body: String, status: StatusCode) -> Result<Response<Body>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::CONTENT_TYPE, mime::TEXT_HTML.as_ref())
        .body(Body::from(body))
        .map_err(Error::from)
}

/// A handlebars HTML template.
static HTML_TEMPLATE: &str = include_str!("template.html");

/// The data for the handlebars HTML template. Handlebars will use serde to get
/// the data out of the struct and mapped onto the template.
#[derive(Serialize)]
struct HtmlCfg {
    title: String,
    body: String,
}

/// Render an HTML page with handlebars, the template and the configuration data.
fn render_html(cfg: HtmlCfg) -> Result<String> {
    let reg = Handlebars::new();
    let rendered = reg
        .render_template(HTML_TEMPLATE, &cfg)
        .map_err(|source| Error::TemplateRender { source })?;
    Ok(rendered)
}

/// Render an HTML page from an HTTP status code
fn render_error_html(status: StatusCode) -> Result<String> {
    render_html(HtmlCfg {
        title: format!("{}", status),
        body: String::new(),
    })
}

/// A custom `Result` typedef
pub type Result<T> = std::result::Result<T, Error>;

/// The basic-http-server error type.
///
/// This is divided into two types of errors: "semantic" errors and "blanket"
/// errors. Semantic errors are custom to the local application semantics and
/// are usually preferred, since they add context and meaning to the error
/// chain. They don't require boilerplate `From` implementations, but do require
/// `map_err` to create when they have interior `causes`.
///
/// Blanket errors are just wrappers around other types, like `Io(io::Error)`.
/// These are common errors that occur in many places so are easier to code and
/// maintain, since e.g. every occurrence of an I/O error doesn't need to be
/// given local semantics.
///
/// The criteria of when to use which type of error variant, and their pros and
/// cons, aren't obvious.
///
/// These errors use `derive(Error)` from the `thiserror` crate to reduce
/// boilerplate.
#[derive(Debug, Error)]
pub enum Error {
    // blanket "pass-through" error types
    #[error("Extension error")]
    Ext {
        #[from]
        source: ext::Error,
    },

    #[error("HTTP error")]
    Http {
        #[from]
        source: http::Error,
    },

    #[error("Hyper error")]
    Hyper {
        #[from]
        source: hyper::Error,
    },

    #[error("I/O error")]
    Io {
        #[from]
        source: io::Error,
    },

    // custom "semantic" error types
    #[error("failed to parse IP address")]
    AddrParse { source: std::net::AddrParseError },

    #[error("failed to render template")]
    TemplateRender {
        source: handlebars::TemplateRenderError,
    },

    #[error("failed to convert URL to local file path")]
    UrlToPath,
}
