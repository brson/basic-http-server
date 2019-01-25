/*

A simple HTTP server that serves static content from a given directory,
built on [hyper].

It creates a hyper HTTP server, which uses non-blocking network I/O on
top of [tokio] internally. Files are read sequentially, without using
async I/O, by futures running in a thread pool (using [futures_cpupool]).

[hyper]: https://github.com/hyperium/hyper
[tokio]: https://tokio.rs/
[futures_cpupool]: https://github.com/alexcrichton/futures-rs/tree/master/futures-cpupool

*/

// The error_type! macro to avoid boilerplate trait
// impls for error handling.
#[macro_use]
extern crate error_type;

use clap::App;
use futures::{future::Either, Future};
use http::status::StatusCode;
use hyper::{header, service::service_fn, Body, Request, Response, Server};
use std::{
    error::Error as StdError,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
};
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
    // Create the configuration from the command line arguments. It
    // includes the IP address and port to listen on and the path to use
    // as the HTTP server's root directory
    let config = parse_config_from_cmdline()?;
    let Config { addr, root_dir, .. } = config;

    // Create HTTP service, passing the document root directory
    let server = Server::bind(&addr)
        .serve(move || {
            let root_dir = root_dir.clone();
            service_fn(move |req| serve(root_dir.clone(), req))
        })
        .map_err(|e| {
            println!("There was an error: {}", e);
            ()
        });

    hyper::rt::run(server);
    Ok(())
}

// The configuration object, created from command line options
#[derive(Clone)]
struct Config {
    addr: SocketAddr,
    root_dir: PathBuf,
}

fn parse_config_from_cmdline() -> Result<Config, Error> {
    let matches = App::new("basic-http-server")
        .version("0.1")
        .about("A basic HTTP file server")
        .args_from_usage(
            "[ROOT] 'Sets the root dir (default \".\")'
             [ADDR] -a --addr=[ADDR] 'Sets the IP:PORT combination (default \"127.0.0.1:4000\")'",
        )
        .get_matches();

    let addr = matches.value_of("ADDR").unwrap_or("127.0.0.1:4000");
    let root_dir = matches.value_of("ROOT").unwrap_or(".");

    // Display the configuration to be helpful
    println!("addr: http://{}", addr);
    println!("root dir: {:?}", root_dir);
    println!("");

    Ok(Config {
        addr: addr.parse()?,
        root_dir: PathBuf::from(root_dir),
    })
}

fn serve(
    root_dir: PathBuf,
    req: Request<Body>,
) -> impl Future<Item = Response<Body>, Error = Error> {
    let uri_path = req.uri().path();
    if let Some(path) = local_path_for_request(&uri_path, &root_dir) {
        let future =
            tokio_fs::file::File::open(path.clone()).then(|open_result| match open_result {
                Ok(file) => {
                    let buf: Vec<u8> = Vec::new();
                    let future = tokio_io::io::read_to_end(file, buf)
                        .map_err(Error::Io)
                        .and_then(move |(_, buf)| {
                            let mime_type = file_path_mime(&path);
                            Response::builder()
                                .status(StatusCode::OK)
                                .header(header::CONTENT_LENGTH, buf.len() as u64)
                                .header(header::CONTENT_TYPE, mime_type.as_ref())
                                .body(Body::from(buf))
                                .map_err(Error::from)
                        });
                    Either::A(future)
                }
                Err(e) => {
                    let future = match e.kind() {
                        io::ErrorKind::NotFound => Either::A(futures::future::result(
                            Response::builder()
                                .status(StatusCode::NOT_FOUND)
                                .body(Body::empty()),
                        )),
                        _ => Either::B(internal_server_error()),
                    };
                    Either::B(future.map_err(Error::from))
                }
            });
        Either::A(future)
    } else {
        Either::B(internal_server_error().map_err(Error::from))
    }
}

fn file_path_mime(file_path: &Path) -> mime::Mime {
    let mime_type = match file_path.extension().and_then(std::ffi::OsStr::to_str) {
        Some("html") => mime::TEXT_HTML,
        Some("css") => mime::TEXT_CSS,
        Some("js") => mime::TEXT_JAVASCRIPT,
        Some("jpg") => mime::IMAGE_JPEG,
        Some("png") => mime::IMAGE_PNG,
        _ => mime::TEXT_PLAIN,
    };
    mime_type
}

fn local_path_for_request(request_path: &str, root_dir: &Path) -> Option<PathBuf> {
    // This is equivalent to checking for hyper::RequestUri::AbsoluteUri
    if !request_path.starts_with("/") {
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
        return None;
    }

    // Maybe turn directory requests into index.html requests
    if request_path.ends_with('/') {
        path.push("index.html");
    }

    Some(path)
}

fn internal_server_error() -> impl Future<Item = Response<Body>, Error = http::Error> {
    futures::future::result(
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_LENGTH, 0)
            .body(Body::empty()),
    )
}

// The custom Error type that encapsulates all the possible errors
// that can occur in this crate. This macro defines it and
// automatically creates Display, Error, and From implementations for
// all the variants.
error_type! {
    #[derive(Debug)]
    enum Error {
        Io(io::Error) { },
        HttpError(http::Error) { },
        AddrParse(std::net::AddrParseError) { },
        Std(Box<StdError + Send + Sync>) {
            desc (e) e.description();
        },
        ParseInt(std::num::ParseIntError) { },
    }
}
