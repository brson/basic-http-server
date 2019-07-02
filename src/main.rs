//! A simple HTTP server, for learning and local doc development.

#[macro_use]
extern crate derive_more;
#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_derive;

use env_logger::{Builder, Env};
use futures::{future, future::Either, Future};
use handlebars::Handlebars;
use http::status::StatusCode;
use http::Uri;
use hyper::{header, service::service_fn, Body, Request, Response, Server};
use std::{
    error::Error as StdError,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use structopt::StructOpt;
use tokio::fs::File;

// Developer extensions
mod ext;

fn main() {
    // Set up our error handling immediately. The situations in which `run` can
    // actually return errors are few though - any errors propagated up to the
    // hyper request handler silently cause the connection to be closed, and our
    // HTTP service additionally converts any errors to HTTP error responses.
    if let Err(e) = run() {
        log_error_chain(&e);
    }
}

/// Basic error reporting, including the "cause chain". This is used both by the
/// top-level error reporting and to report internal server errors.
fn log_error_chain(mut e: &dyn StdError) {
    error!("error: {}", e);
    while let Some(source) = e.source() {
        error!("caused by: {}", source);
        e = source;
    }
}

fn run() -> Result<()> {
    // Initialize logging, and log the "info" level for this crate only, unless
    // the environment contains `RUST_LOG`.
    let env = Env::new().default_filter_or("basic_http_server=info");
    Builder::from_env(env)
        .default_format_module_path(false)
        .default_format_timestamp(false)
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

    let server = Server::try_bind(&config.addr)
        .map_err(|e| translate_bind_error(e, config.addr))?
        .serve(move || {
            let config = config.clone();
            service_fn(move |req| {
                serve(&config, req).map_err(|e| {
                    // Log any errors that result from handling a single HTTP
                    // request. This _should_ be impossible - we expect our
                    // service function to map all errors to HTTP error
                    // responses.
                    error!("request handler error: {}", e);
                    e
                })
            })
        })
        .map_err(|e| {
            // Log any errors that result from hyper's `Server` future failing.
            // The tokio runtime expects to run a future that doesn't error so
            // not sure how to square that with hyper's `Server` carrying an
            // error type, but here hyper's error type is mapped to nil.
            error!("server error: {}", e);
            ()
        });

    tokio::run(server);

    Ok(())
}

/// The configuration object, parsed from command line options
#[derive(Clone, StructOpt)]
#[structopt(about = "A basic HTTP file server")]
pub struct Config {
    /// Sets the IP:PORT combination
    #[structopt(
        name = "ADDR",
        short = "a",
        long = "addr",
        parse(try_from_str),
        default_value = "127.0.0.1:4000"
    )]
    addr: SocketAddr,
    /// Sets the root dir
    #[structopt(name = "ROOT", parse(from_os_str), default_value = ".")]
    root_dir: PathBuf,
    /// Enable developer extensions
    #[structopt(short = "x")]
    use_extensions: bool,
}

/// Translate a hyper error into our error for binding
fn translate_bind_error(e: hyper::Error, addr: SocketAddr) -> Error {
    if let Some(os_error) = e
        .source()
        .and_then(|source| source.downcast_ref::<io::Error>())
    {
        if os_error.kind() == io::ErrorKind::AddrInUse {
            return Error::AddrInUse(addr);
        }
    }
    Error::BindWithHyper(e)
}

/// The function that returns a future of an HTTP response for each hyper
/// Request that is received. Errors are turned into an Error response (404 or
/// 500), and never propagated upward for hyper to deal with.
fn serve(config: &Config, req: Request<Body>) -> impl Future<Item = Response<Body>, Error = Error> {
    let config = config.clone();
    serve_file(&req, &config.root_dir)
        .then(
            // Give developer extensions an opportunity to post-process the request/response pair
            move |resp| ext::serve(config, req, resp).map_err(Error::from),
        )
        .then(|maybe_resp| {
            // Turn any errors into an HTTP error response.
            //
            // This `Either` future is a simple way to create a concrete future
            // (i.e. a non-boxed future) of one of two different `Future` types.
            // We'll use it a lot.
            //
            // Here type `A` is a `FutureResult`, and type `B` is some `impl Future`
            // returned by `make_error_response`.
            match maybe_resp {
                Ok(r) => Either::A(future::ok(r)),
                Err(e) => Either::B(make_error_response(e)),
            }
        })
}

/// Serve static files from a root directory
fn serve_file(
    req: &Request<Body>,
    root_dir: &PathBuf,
) -> impl Future<Item = Response<Body>, Error = Error> {
    let uri = req.uri().clone();
    let root_dir = root_dir.clone();

    // First, try to do a redirect per `try_dir_redirect`. If that doesn't
    // happen, then find the path to the static file we want to serve - which
    // may be `index.html` for directories - and send a response containing that
    // file.
    try_dir_redirect(req, &root_dir).and_then(move |maybe_redir_resp| {
        if let Some(redir_resp) = maybe_redir_resp {
            return Either::A(future::ok(redir_resp));
        }

        if let Some(path) = local_path_with_maybe_index(&uri, &root_dir) {
            Either::B(
                File::open(path.clone())
                    .map_err(Error::from)
                    .and_then(move |file| respond_with_file(file, path)),
            )
        } else {
            Either::A(future::err(Error::UrlToPath))
        }
    })
}

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
fn try_dir_redirect(
    req: &Request<Body>,
    root_dir: &PathBuf,
) -> impl Future<Item = Option<Response<Body>>, Error = Error> {
    if !req.uri().path().ends_with("/") {
        debug!("path does not end with /");
        if let Some(path) = local_path_for_request(req.uri(), root_dir) {
            if path.is_dir() {
                let mut new_loc = req.uri().path().to_string();
                new_loc.push_str("/");
                if let Some(query) = req.uri().query() {
                    new_loc.push_str("?");
                    new_loc.push_str(query);
                }
                info!("redirecting {} to {}", req.uri(), new_loc);
                future::result(
                    Response::builder()
                        .status(StatusCode::FOUND)
                        .header(header::LOCATION, new_loc)
                        .body(Body::empty())
                        .map(Some)
                        .map_err(Error::from),
                )
            } else {
                future::ok(None)
            }
        } else {
            future::err(Error::UrlToPath)
        }
    } else {
        future::ok(None)
    }
}

/// Read the file completely and construct a 200 response with that file as the
/// body of the response. If the I/O here fails then an error future will be
/// returned, and `serve` will convert it into the appropriate HTTP error
/// response.
fn respond_with_file(
    file: tokio::fs::File,
    path: PathBuf,
) -> impl Future<Item = Response<Body>, Error = Error> {
    read_file(file).and_then(move |buf| {
        let mime_type = file_path_mime(&path);
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, buf.len() as u64)
            .header(header::CONTENT_TYPE, mime_type.as_ref())
            .body(Body::from(buf))
            .map_err(Error::from)
    })
}

/// Read a file and return a future of the buffer
fn read_file(file: tokio::fs::File) -> impl Future<Item = Vec<u8>, Error = Error> {
    let buf: Vec<u8> = Vec::new();
    tokio::io::read_to_end(file, buf)
        .map_err(Error::Io)
        .and_then(|(_read_handle, buf)| future::ok(buf))
}

/// Get a MIME type based on the file etension
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
    let request_path = uri.path();

    debug!("raw URI to path: {}", request_path);

    // This is equivalent to checking for hyper::RequestUri::AbsoluteUri
    if !request_path.starts_with("/") {
        debug!("found non-absolute path");
        return None;
    }

    // Trim off the url parameters starting with '?'
    let end = request_path.find('?').unwrap_or(request_path.len());
    let request_path = &request_path[0..end];

    // Append the requested path to the root directory
    let mut path = root_dir.to_owned();
    if request_path.starts_with('/') {
        path.push(&request_path[1..]);
    } else {
        debug!("found non-absolute path");
        return None;
    }

    debug!("URL · path : {} · {}", uri, path.display());

    Some(path)
}

/// Convert an error to an HTTP error response future, with correct response code.
fn make_error_response(e: Error) -> impl Future<Item = Response<Body>, Error = Error> {
    match e {
        Error::Io(e) => Either::A(make_io_error_response(e)),
        e => Either::B(make_internal_server_error_response(e)),
    }
}

/// Convert an error into a 500 internal server error, and log it.
fn make_internal_server_error_response(
    err: Error,
) -> impl Future<Item = Response<Body>, Error = Error> {
    log_error_chain(&err);
    make_error_response_from_code(StatusCode::INTERNAL_SERVER_ERROR)
}

/// Handle the one special io error (file not found) by returning a 404, otherwise
/// return a 500.
fn make_io_error_response(error: io::Error) -> impl Future<Item = Response<Body>, Error = Error> {
    match error.kind() {
        io::ErrorKind::NotFound => {
            debug!("{}", error);
            Either::A(make_error_response_from_code(StatusCode::NOT_FOUND))
        }
        _ => Either::B(make_internal_server_error_response(Error::Io(error))),
    }
}

/// Make an error response given an HTTP status code.
fn make_error_response_from_code(
    status: StatusCode,
) -> impl Future<Item = Response<Body>, Error = Error> {
    future::result({ render_error_html(status) })
        .and_then(move |body| html_str_to_response(body, status))
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

/// A handlebars HTML template
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
        .map_err(Error::TemplateRender)?;
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

/// The basic-http-server error type
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
/// These errors use `derive(Display)` from the `derive-more` crate to reduce
/// boilerplate.
#[derive(Debug, Display)]
pub enum Error {
    // blanket "pass-through" error types
    #[display(fmt = "HTTP error")]
    Http(http::Error),

    #[display(fmt = "I/O error")]
    Io(io::Error),

    // custom "semantic" error types
    #[display(fmt = "failed to parse IP address")]
    AddrParse(std::net::AddrParseError),

    #[display(fmt = "the address \"{}\" is already in use", _0)]
    AddrInUse(SocketAddr),

    #[display(fmt = "failed to bind server to socket")]
    BindWithHyper(hyper::Error),

    #[display(fmt = "markdown is not UTF-8")]
    MarkdownUtf8,

    #[display(fmt = "failed to strip prefix in directory listing")]
    StripPrefixInDirList(std::path::StripPrefixError),

    #[display(fmt = "failed to render template")]
    TemplateRender(handlebars::TemplateRenderError),

    #[display(fmt = "failed to convert URL to local file path")]
    UrlToPath,

    #[display(fmt = "formatting error while creating directory listing")]
    WriteInDirList(std::fmt::Error),
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        use Error::*;

        match self {
            Http(e) => Some(e),
            Io(e) => Some(e),
            AddrParse(e) => Some(e),
            AddrInUse(_) => None,
            BindWithHyper(e) => Some(e),
            MarkdownUtf8 => None,
            StripPrefixInDirList(e) => Some(e),
            TemplateRender(e) => Some(e),
            UrlToPath => None,
            WriteInDirList(e) => Some(e),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

impl From<http::Error> for Error {
    fn from(e: http::Error) -> Error {
        Error::Http(e)
    }
}
