/*

A simple HTTP server that serves static content from a given directory,
built on [hyper].

It creates a hyper HTTP server, which uses non-blocking network I/O on
top of [tokio] internally.

[hyper]: https://github.com/hyperium/hyper
[tokio]: https://tokio.rs/
*/

#[macro_use]
extern crate err_derive;
#[macro_use]
extern crate derive_more;
#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_derive;

use clap::App;
use env_logger::{Builder, Env};
use futures::{future, future::Either, Future};
use handlebars::Handlebars;
use http::Uri;
use http::status::StatusCode;
use hyper::{header, service::service_fn, Body, Request, Response, Server};
use std::{
    error::Error as StdError,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use tokio::fs::File;

mod ext;

fn main() {
    // Set up our error handling immediatly. Everything in this crate
    // that can return an error returns our custom Error type. `?`
    // will convert from all other error types by our `From<SomeError>
    // to Error` implementations. Every time a conversion doesn't
    // exist the compiler will tell us to create it. This crate uses
    // the `error_type!` macro to reduce error boilerplate.
    if let Err(e) = run() {
        println!("error: {}", e.description());
    }
}

fn run() -> Result<(), Error> {
    // Initialize logging, log the "info" level for this crate only, unless the
    // environment contains `RUST_LOG`.
    let env = Env::new().default_filter_or("basic_http_server=info");
    Builder::from_env(env)
        .default_format_module_path(false)
        .default_format_timestamp(false)
        .init();

    // Create the configuration from the command line arguments. It
    // includes the IP address and port to listen on and the path to use
    // as the HTTP server's root directory
    let config = parse_config_from_cmdline()?;

    // Display the configuration to be helpful
    info!("basic-http-server {}", env!("CARGO_PKG_VERSION"));
    info!("addr: http://{}", config.addr);
    info!("root dir: {}", config.root_dir.display());
    info!("extensions: {}", config.use_extensions);
    info!("");

    let Config { addr, .. } = config;

    // Create HTTP service, passing the document root directory
    let server = Server::bind(&addr)
        .serve(move || {
            let config = config.clone();
            service_fn(move |req| {
                let config = config.clone();
                serve(&config, req)
            })
        }).map_err(|e| {
            // TODO how to handle this case correctly?
            error!("server returned error: {}", e);
            ()
        });

    tokio::run(server);

    Ok(())
}

// The configuration object, created from command line options
#[derive(Clone)]
pub struct Config {
    addr: SocketAddr,
    root_dir: PathBuf,
    use_extensions: bool,
}

fn parse_config_from_cmdline() -> Result<Config, Error> {
    let matches = App::new("basic-http-server")
        .version(env!("CARGO_PKG_VERSION"))
        .about("A basic HTTP file server")
        .args_from_usage(
            "[ROOT] 'Sets the root dir (default \".\")'
             [ADDR] -a --addr=[ADDR] 'Sets the IP:PORT combination (default \"127.0.0.1:4000\")',
             [EXT] -x 'Enable dev extensions'",
        )
        .get_matches();

    let addr = matches.value_of("ADDR").unwrap_or("127.0.0.1:4000");
    let root_dir = matches.value_of("ROOT").unwrap_or(".");
    let ext = matches.is_present("EXT");

    Ok(Config {
        addr: addr.parse()?,
        root_dir: PathBuf::from(root_dir),
        use_extensions: ext,
    })
}

// The function that returns a future of http responses for each hyper Request
// that is received. Errors are turned into an Error response (404 or 500).
fn serve(
    config: &Config,
    req: Request<Body>,
) -> impl Future<Item = Response<Body>, Error = Error> {
    let config = config.clone();
    serve_file(&req, &config.root_dir).then({
        move |resp| {
            ext::serve(config, req, resp)
        }
    }).then(|maybe_resp| {
        match maybe_resp {
            Ok(r) => Either::A(future::ok(r)),
            Err(e) => Either::B(make_error_response(e)),
        }
    })
}

fn serve_file(
    req: &Request<Body>,
    root_dir: &PathBuf,
) -> impl Future<Item = Response<Body>, Error = Error> {
    let uri = req.uri().clone();
    let root_dir = root_dir.clone();
    try_dir_redirect(req, &root_dir).and_then(move |maybe_resp| {
        if let Some(resp) = maybe_resp {
            return Either::A(future::ok(resp));
        }

        if let Some(path) = local_path_with_maybe_index(&uri, &root_dir) {
            let err_path = path.clone();
            Either::B(File::open(path.clone()).map_err(move |e| {
                if e.kind() == io::ErrorKind::NotFound {
                    debug!("file {} not found", err_path.display());
                }
                Error::from(e)
            }).and_then(move |file| {
                respond_with_file(file, path)
            }))
        } else {
            Either::A(future::err(Error::UrlToPath))
        }
    })
}

/// If we get a URL without trailing "/" that can be mapped to a directory, then
/// return a 302 redirect to the path with the trailing "/". For the purpose of
/// building absolute URLs from relative URLs, agents only treat paths with
/// trailing "/" as directories, so we have to redirect to the proper URL first.
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
                        .map_err(Error::from)
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

// Read the file completely and construct a 200 response with that file as
// the body of the response.
fn respond_with_file(
    file: tokio::fs::File,
    path: PathBuf,
) -> impl Future<Item = Response<Body>, Error = Error> {
    read_file(file)
        .and_then(move |buf| {
            let mime_type = file_path_mime(&path);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_LENGTH, buf.len() as u64)
                .header(header::CONTENT_TYPE, mime_type.as_ref())
                .body(Body::from(buf))
                .map_err(Error::from)
        })
}

fn read_file(
    file: tokio::fs::File,
) -> impl Future<Item = Vec<u8>, Error = Error> {
    let buf: Vec<u8> = Vec::new();
    tokio::io::read_to_end(file, buf)
        .map_err(Error::Io)
        .and_then(|(_, buf)| future::ok(buf))
}


fn file_path_mime(file_path: &Path) -> mime::Mime {
    let mime_type = match file_path.extension().and_then(std::ffi::OsStr::to_str) {
        Some("html") => mime::TEXT_HTML,
        Some("css") => mime::TEXT_CSS,
        Some("js") => mime::TEXT_JAVASCRIPT,
        Some("jpg") => mime::IMAGE_JPEG,
        Some("md") => "text/markdown; charset=UTF-8".parse::<mime::Mime>().unwrap(),
        Some("png") => mime::IMAGE_PNG,
        Some("svg") => mime::IMAGE_SVG,
        Some("wasm") => "application/wasm".parse::<mime::Mime>().unwrap(),
        _ => mime::TEXT_PLAIN,
    };
    mime_type
}

fn local_path_with_maybe_index(uri: &Uri, root_dir: &Path) -> Option<PathBuf> {
    local_path_for_request(uri, root_dir)
        .map(|mut p: PathBuf| {
            if p.is_dir() {
                p.push("index.html");
                debug!("trying {} for directory URL", p.display());
            } else {
                trace!("trying path as from URL");
            }
            p
        })
}

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

fn make_error_response(e: Error) -> impl Future<Item = Response<Body>, Error = Error> {
    match e {
        Error::Io(e) => {
            Either::A(handle_io_error(e))
        }
        _ => {
            Either::B(internal_server_error())
        }
    }
}

fn internal_server_error() -> impl Future<Item = Response<Body>, Error = Error> {
    error_response(StatusCode::INTERNAL_SERVER_ERROR)
}

// Handle the one special io error (file not found) by returning a 404, otherwise
// return a 500
fn handle_io_error(error: io::Error) -> impl Future<Item = Response<Body>, Error = Error> {
    match error.kind() {
        io::ErrorKind::NotFound => Either::A(
            error_response(StatusCode::NOT_FOUND)
        ),
        _ => Either::B(internal_server_error()),
    }
}

fn error_response(status: StatusCode)
-> impl Future<Item = Response<Body>, Error = Error> {
    future::result({
        render_error_html(status)
    }).and_then(move |body| {
        Response::builder()
            .status(status)
            .header(header::CONTENT_LENGTH, body.len())
            .body(Body::from(body))
            .map_err(Error::from)
    })
}

static HTML_TEMPLATE: &str = include_str!("template.html");

#[derive(Serialize)]
struct HtmlCfg {
    title: String,
    body: String,
}

fn render_html(cfg: HtmlCfg) -> Result<String, Error> {
    let reg = Handlebars::new();
    Ok(reg.render_template(HTML_TEMPLATE, &cfg)?)
}

fn render_error_html(status: StatusCode) -> Result<String, Error> {
    render_html(HtmlCfg {
        title: format!("{}", status),
        body: String::new(),
    })
}

// The custom Error type that encapsulates all the possible errors
// that can occur in this crate. This macro defines it and
// automatically creates Display, Error, and From implementations for
// all the variants.
//
// TODO: Make these more semantic
#[derive(From, Error, Debug)]
pub enum Error {
    #[error(display = "failed to render template")]
    Handlebars(#[error(cause)] handlebars::TemplateRenderError),

    #[error(display = "i/o error")]
    Io(#[error(cause)] io::Error),

    #[error(display = "http error")]
    HttpError(#[error(cause)] http::Error),

    #[error(display = "failed to parse IP address")]
    AddrParse(#[error(cause)] std::net::AddrParseError),

    #[error(display = "failed to parse a number")]
    ParseInt(#[error(cause)] std::num::ParseIntError),

    #[error(display = "failed to parse a boolean")]
    ParseBool(#[error(cause)] std::str::ParseBoolError),

    #[error(display = "string is not UTF-8")]
    ParseUtf8(#[error(cause)] std::string::FromUtf8Error),

    #[error(display = "markdown is not UTF-8")]
    MarkdownUtf8,

    #[error(display = "failed to convert URL to local file path")]
    UrlToPath,

    #[error(display = "formatting error")]
    Fmt(#[error(cause)] std::fmt::Error),

    #[error(display = "failed to strip prefix")]
    StripPrefix(#[error(cause)] std::path::StripPrefixError),
}
