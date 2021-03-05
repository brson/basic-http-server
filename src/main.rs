//! A simple HTTP server, for learning and local development.

#[macro_use]
extern crate derive_more;

use bytes::BytesMut;
use env_logger::{Builder, Env};
use futures::future;
use futures::stream::StreamExt;
use futures::FutureExt;
use handlebars::Handlebars;
use http::header::{self, HeaderMap, HeaderValue};
use http::status::StatusCode;
use http::Uri;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server};
use log::{debug, error, info, trace, warn};
use percent_encoding::percent_decode_str;
use serde::Serialize;
use std::error::Error as StdError;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str;
use std::sync::Arc;
use structopt::StructOpt;
use tokio::codec::{BytesCodec, FramedRead};
use tokio::fs::File;
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
fn log_error_chain(mut e: &dyn StdError) {
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
    /// The IP:PORT combination
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

    /// Enable developer extensions
    #[structopt(short = "x")]
    use_extensions: bool,

    /// Allow serving files outside the root given by ROOT, meaining all your files are accessible.
    ///
    /// This allows access to *all files* on your computer, so don't use this on untrusted networks
    /// like the internet.
    #[structopt(long = "allow-escape-root")]
    allow_escape_root: bool,

    /// Enable basic http auth with the given password
    #[structopt(long = "auth", parse(try_from_str))]
    auth: Option<Auth>,
}

impl Config {
    /// Ensure that the `root_dir` is a canonical absolute path with no `.` or `..` in.
    fn canonical_root_dir(&mut self) -> Result<()> {
        // This line of code takes what might be a relative path and returns an absolute path
        // without any `.` or `..` in. If the path then points to a symbolic link, the code then
        // follows the link, repeating until it finds something which is not a symbolic link. This
        // is what is set as the `root_dir`.
        //
        // Doing this makes it possible to report the real root directory in the log, and also
        // makes checking that a file is actually in the root directory more robust.
        let canonical_root_dir = self.root_dir.canonicalize()?;
        self.root_dir = canonical_root_dir;
        Ok(())
    }

    /// Checks if the given path is in the root dir.
    ///
    /// If it is, return its canonical representation. If it isn't, return an error. This function
    /// will error if the path does not point to an actual file or directory.
    fn check_in_root_dir(&self, path: PathBuf) -> Result<PathBuf> {
        let path = path.canonicalize()?;

        // Skip the check if we've configured to allow files outside the root.
        if self.allow_escape_root || path.starts_with(&self.root_dir) {
            Ok(path)
        } else {
            return Err(Error::EntityNotInRoot);
        }
    }

    /// Check if the request has the required password (if we set one).
    fn check_auth(&self, req: &Request<Body>) -> bool {
        // This macro avoids us having to write out a match for every step.
        macro_rules! err_to_ret {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(_) => return false,
                }
            };
        }

        let reference_auth = match self.auth.as_ref() {
            Some(auth) => auth,
            // If there is no password, carry on serving the request.
            None => return true,
        };

        // Get and decode the auth token
        let headers = req.headers();
        let auth_header = match headers.get(header::AUTHORIZATION) {
            Some(header) => header,
            // If the header isn't set, then send a request for auth.
            None => return false,
        };
        let auth_header = err_to_ret!(auth_header.to_str());

        if !matches!(auth_header.get(..6), Some(s) if s.eq_ignore_ascii_case("basic ")) {
            return false;
        }
        let auth = match auth_header.get(6..).map(|s| s.trim()) {
            Some(auth) => auth,
            None => return false,
        };
        let auth = err_to_ret!(base64::decode(auth));
        let auth = err_to_ret!(str::from_utf8(&auth));
        let auth: Auth = err_to_ret!(auth.parse());
        *reference_auth == auth
    }
}

#[derive(Clone, PartialEq)]
struct Auth {
    username: String,
    password: String,
}

impl std::str::FromStr for Auth {
    type Err = &'static str;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        // Split into <username> : <password>
        let mut iter = s.splitn(2, ':');
        let username = iter.next().unwrap(); // cannot fail
        let password = iter.next().unwrap_or("");

        Ok(Auth {
            username: username.to_owned(),
            password: password.to_owned(),
        })
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
    let mut config = Config::from_args();
    config.canonical_root_dir()?;
    // Put `config` in an `Arc`. This means we can't mutate `config` any more, but
    // also means it is very cheap to clone, since all clones point to the same data.
    let config = Arc::new(config);

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
    let server = Server::try_bind(&config.addr)?.serve(make_service);

    // Create a Tokio runtime and block on Hyper forever.
    let rt = Runtime::new()?;
    rt.block_on(server)?;

    Ok(())
}

/// Create an HTTP Response future for each Request.
///
/// Errors are turned into an appropriate HTTP error response, and never
/// propagated upward for hyper to deal with.
async fn serve(config: Arc<Config>, req: Request<Body>) -> Response<Body> {
    // Serve the requested file.
    let resp = serve_or_error(config, req).await;

    // Transform internal errors to error responses.
    let resp = transform_error(resp);

    resp
}

/// Handle all types of requests, but don't deal with transforming internal
/// errors to HTTP error responses.
async fn serve_or_error(config: Arc<Config>, req: Request<Body>) -> Result<Response<Body>> {
    // This server only supports the GET method. Return an appropriate
    // response otherwise.
    if let Some(resp) = handle_unsupported_request(&req) {
        return resp;
    }

    // If there is a password, check the password. Return unauthorized if it was missing/incorrect
    if !config.check_auth(&req) {
        let mut auth_value =
            HeaderValue::from_static(r#"Basic relm="User Visible Realm", charset="UTF-8""#);
        auth_value.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(header::WWW_AUTHENTICATE, auth_value);
        return make_error_response_from_code_and_headers(StatusCode::UNAUTHORIZED, headers);
    }

    // Serve the requested file.
    // Here we pass a `&PathBuf` to a function expecting a `&Path`. This works because of *deref
    // coercions*, in this case meaning that `PathBuf` implements `Deref` with `Target=Path`.
    let resp = serve_file(&req, &config).await;

    // Give developer extensions an opportunity to post-process the request/response pair.
    let resp = ext::serve(config, req, resp).await;

    resp
}

/// Serve static files from a root directory.
async fn serve_file(req: &Request<Body>, config: &Config) -> Result<Response<Body>> {
    // First, try to do a redirect. If that doesn't happen, then find the path
    // to the static file we want to serve - which may be `index.html` for
    // directories - and send a response containing that file.
    let maybe_redir_resp = try_dir_redirect(req, config)?;

    if let Some(redir_resp) = maybe_redir_resp {
        return Ok(redir_resp);
    }

    let path = local_path_with_maybe_index(req.uri(), config)?;

    Ok(respond_with_file(&path, config).await?)
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
fn try_dir_redirect(req: &Request<Body>, config: &Config) -> Result<Option<Response<Body>>> {
    if req.uri().path().ends_with("/") {
        return Ok(None);
    }

    debug!("path does not end with /");

    let path = local_path_for_request(req.uri(), config)?;

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
}

/// Construct a 200 response with the file as the body, streaming it to avoid
/// loading it fully into memory.
///
/// If the I/O here fails then an error future will be returned, and `serve`
/// will convert it into the appropriate HTTP error response.
async fn respond_with_file(path: &Path, config: &Config) -> Result<Response<Body>> {
    config.check_in_root_dir(path.to_owned())?;

    let mime_type = file_path_mime(&path);

    let file = File::open(path).await?;

    let meta = file.metadata().await?;
    let len = meta.len();

    // Here's the streaming code. How to do this isn't documented in the
    // Tokio/Hyper API docs. Codecs are how Tokio creates Streams; a FramedRead
    // turns an AsyncRead plus a Decoder into a Stream; and BytesCodec is a
    // Decoder. FramedRead though creates a Stream<Result<BytesMut>> and Hyper's
    // Body wants a Stream<Result<Bytes>>, and BytesMut::freeze will give us a
    // Bytes.

    let codec = BytesCodec::new();
    let stream = FramedRead::new(file, codec);
    let stream = stream.map(|b| b.map(BytesMut::freeze));
    let body = Body::wrap_stream(stream);

    let resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, len as u64)
        .header(header::CONTENT_TYPE, mime_type.as_ref())
        .body(body)?;

    Ok(resp)
}

/// Get a MIME type based on the file extension.
///
/// If the extension is unknown then return "application/octet-stream".
fn file_path_mime(file_path: &Path) -> mime::Mime {
    mime_guess::from_path(file_path).first_or_octet_stream()
}

/// Find the local path for a request URI, converting directories to the
/// `index.html` file.
fn local_path_with_maybe_index(uri: &Uri, config: &Config) -> Result<PathBuf> {
    local_path_for_request(uri, config).map(|mut p: PathBuf| {
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
fn local_path_for_request(uri: &Uri, config: &Config) -> Result<PathBuf> {
    debug!("raw URI: {}", uri);

    let request_path = uri.path();

    debug!("raw URI to path: {}", request_path);

    // Trim off the url parameters starting with '?'
    let end = request_path.find('?').unwrap_or(request_path.len());
    let request_path = &request_path[0..end];

    // Convert %-encoding to actual values
    let decoded = percent_decode_str(&request_path);
    let request_path = if let Ok(p) = decoded.decode_utf8() {
        p
    } else {
        error!("non utf-8 URL: {}", request_path);
        return Err(Error::UriNotUtf8);
    };

    // Append the requested path to the root directory
    let mut path = config.root_dir.to_owned();
    if request_path.starts_with('/') {
        path.push(&request_path[1..]);
    } else {
        warn!("found non-absolute path {}", request_path);
        return Err(Error::UriNotAbsolute);
    }

    debug!("URL · path : {} · {}", uri, path.display());

    Ok(path)
}

/// Create an error response if the request contains unsupported methods,
/// headers, etc.
fn handle_unsupported_request(req: &Request<Body>) -> Option<Result<Response<Body>>> {
    get_unsupported_request_message(req)
        .map(|unsup| make_error_response_from_code_and_headers(unsup.code, unsup.headers))
}

/// Description of an unsupported request.
struct Unsupported {
    code: StatusCode,
    headers: HeaderMap,
}

/// Create messages for unsupported requests.
fn get_unsupported_request_message(req: &Request<Body>) -> Option<Unsupported> {
    use std::iter::FromIterator;

    // https://tools.ietf.org/html/rfc7231#section-6.5.5
    if req.method() != Method::GET {
        return Some(Unsupported {
            code: StatusCode::METHOD_NOT_ALLOWED,
            headers: HeaderMap::from_iter(vec![(header::ALLOW, HeaderValue::from_static("GET"))]),
        });
    }

    None
}

/// Turn any errors into an HTTP error response.
fn transform_error(resp: Result<Response<Body>>) -> Response<Body> {
    match resp {
        Ok(r) => r,
        Err(e) => {
            let resp = make_error_response(e);
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

/// Convert an error to an HTTP error response future, with correct response code.
fn make_error_response(e: Error) -> Result<Response<Body>> {
    let resp = match e {
        Error::Io(e) => make_io_error_response(e)?,
        Error::Ext(ext::Error::Io(e)) => make_io_error_response(e)?,
        Error::EntityNotInRoot => make_error_response_from_code(StatusCode::FORBIDDEN)?,
        e => make_internal_server_error_response(e)?,
    };
    Ok(resp)
}

/// Convert an error into a 500 internal server error, and log it.
fn make_internal_server_error_response(err: Error) -> Result<Response<Body>> {
    log_error_chain(&err);
    let resp = make_error_response_from_code(StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(resp)
}

/// Handle the one special IO error (file not found) by returning a 404, otherwise
/// return a 500.
fn make_io_error_response(error: io::Error) -> Result<Response<Body>> {
    let resp = match error.kind() {
        io::ErrorKind::NotFound => {
            debug!("{}", error);
            make_error_response_from_code(StatusCode::NOT_FOUND)?
        }
        _ => make_internal_server_error_response(Error::Io(error))?,
    };
    Ok(resp)
}

/// Make an error response given an HTTP status code.
fn make_error_response_from_code(status: StatusCode) -> Result<Response<Body>> {
    make_error_response_from_code_and_headers(status, HeaderMap::new())
}

/// Make an error response given an HTTP status code and response headers.
fn make_error_response_from_code_and_headers(
    status: StatusCode,
    headers: HeaderMap,
) -> Result<Response<Body>> {
    let body = render_error_html(status)?;
    let resp = html_str_to_response_with_headers(body, status, headers)?;
    Ok(resp)
}

/// Make an HTTP response from a HTML string.
fn html_str_to_response(body: String, status: StatusCode) -> Result<Response<Body>> {
    html_str_to_response_with_headers(body, status, HeaderMap::new())
}

/// Make an HTTP response from a HTML string and response headers.
fn html_str_to_response_with_headers(
    body: String,
    status: StatusCode,
    headers: HeaderMap,
) -> Result<Response<Body>> {
    let mut builder = Response::builder();

    builder.headers_mut().map(|h| h.extend(headers));

    builder
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
/// These errors use `derive(Display)` from the `derive-more` crate to reduce
/// boilerplate.
#[derive(Debug, Display)]
pub enum Error {
    // blanket "pass-through" error types
    #[display(fmt = "Extension error")]
    Ext(ext::Error),

    #[display(fmt = "HTTP error")]
    Http(http::Error),

    #[display(fmt = "Hyper error")]
    Hyper(hyper::Error),

    #[display(fmt = "I/O error")]
    Io(io::Error),

    // custom "semantic" error types
    #[display(fmt = "failed to parse IP address")]
    AddrParse(std::net::AddrParseError),

    #[display(fmt = "failed to render template")]
    TemplateRender(handlebars::TemplateRenderError),

    #[display(fmt = "requested URI is not an absolute path")]
    UriNotAbsolute,

    #[display(fmt = "requested URI is not UTF-8")]
    UriNotUtf8,

    #[display(fmt = "requested file or directory is not in the root directory")]
    EntityNotInRoot,
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        use Error::*;

        match self {
            Ext(e) => Some(e),
            Io(e) => Some(e),
            Http(e) => Some(e),
            Hyper(e) => Some(e),
            AddrParse(e) => Some(e),
            TemplateRender(e) => Some(e),
            UriNotAbsolute | UriNotUtf8 | EntityNotInRoot => None,
        }
    }
}

impl From<ext::Error> for Error {
    fn from(e: ext::Error) -> Error {
        Error::Ext(e)
    }
}

impl From<http::Error> for Error {
    fn from(e: http::Error) -> Error {
        Error::Http(e)
    }
}

impl From<hyper::Error> for Error {
    fn from(e: hyper::Error) -> Error {
        Error::Hyper(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}
