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
use futures::{Async, Future, Poll};
use futures_cpupool::{CpuFuture, CpuPool};
use hyper::{
    header::{ContentLength, ContentType},
    mime,
    server::{Http, Request, Response, Service},
    StatusCode,
};
use std::{
    error::Error as StdError,
    fs::File,
    io::{self, Read},
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
    // includes the IP address and port to listen on, the path to use
    // as the HTTP server's root directory, and the file I/O thread
    // pool.
    let config = parse_config_from_cmdline()?;
    let Config {
        addr,
        root_dir,
        num_file_threads,
        ..
    } = config;

    // Create HTTP service, passing the document root directory and the
    // thread pool used for executing the file reading I/O on.
    let server = Http::new()
        .bind(&addr, move || {
            Ok(HttpService {
                root_dir: root_dir.clone(),
                pool: CpuPool::new(num_file_threads),
            })
        })
        .unwrap();
    server.run().unwrap();
    Ok(())
}

// The configuration object, created from command line options
#[derive(Clone)]
struct Config {
    addr: SocketAddr,
    root_dir: PathBuf,
    num_file_threads: usize,
    num_server_threads: u16,
}

fn parse_config_from_cmdline() -> Result<Config, Error> {
    let matches = App::new("basic-http-server")
        .version("0.1")
        .about("A basic HTTP file server")
        .args_from_usage(
            "[ROOT] 'Sets the root dir (default \".\")'
             [ADDR] -a --addr=[ADDR] 'Sets the IP:PORT combination (default \"127.0.0.1:4000\")'
             [THREADS] -t --threads=[THREADS] 'Sets the number of server threads (default 4)'
             [FILE-THREADS] --file-threads=[FILE-THREADS] 'Sets the number of threads in the file I/O thread pool (default 100)'")
        .get_matches();

    let default_server_threads = 4;
    let default_file_threads = 100;

    let addr = matches.value_of("ADDR").unwrap_or("127.0.0.1:4000");
    let root_dir = matches.value_of("ROOT").unwrap_or(".");
    let num_server_threads = match matches.value_of("THREADS") {
        Some(t) => t.parse()?,
        None => default_server_threads,
    };
    let num_file_threads = match matches.value_of("FILE-THREADS") {
        Some(t) => t.parse()?,
        None => default_file_threads,
    };

    // Display the configuration to be helpful
    println!("addr: http://{}", addr);
    println!("root dir: {:?}", root_dir);
    println!("server threads: {}", num_server_threads);
    println!("file threads: {}", num_file_threads);
    println!("");

    Ok(Config {
        addr: addr.parse()?,
        root_dir: PathBuf::from(root_dir),
        num_file_threads: num_file_threads,
        num_server_threads: num_server_threads,
    })
}

struct HttpService {
    root_dir: PathBuf,
    pool: CpuPool,
}

// The HttpService knows how to build a ResponseFuture for each hyper Request
// that is received. Errors are turned into an Error response (404 or 500).
impl Service for HttpService {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;
    type Future = ResponseFuture;
    fn call(&self, req: Request) -> Self::Future {
        let uri_path = req.uri().path();
        if let Some(path) = local_path_for_request(&uri_path, &self.root_dir) {
            ResponseFuture::File(self.pool.spawn(FileFuture { path }))
        } else {
            ResponseFuture::Error
        }
    }
}

enum ResponseFuture {
    File(CpuFuture<Response, Error>),
    Error,
}

impl Future for ResponseFuture {
    type Item = Response;
    type Error = hyper::Error;
    fn poll(&mut self) -> Poll<Response, hyper::Error> {
        match *self {
            // If this is a File variant, poll the contained CpuFuture
            // and propagate the result outward as a Response.
            ResponseFuture::File(ref mut f) => match f.poll() {
                Ok(Async::Ready(rsp)) => Ok(Async::Ready(rsp)),
                Ok(Async::NotReady) => Ok(Async::NotReady),
                Err(_) => Ok(Async::Ready(internal_server_error())),
            },
            // For the Error variant, we can just return an error immediately.
            ResponseFuture::Error => Ok(Async::Ready(internal_server_error())),
        }
    }
}

struct FileFuture {
    path: PathBuf,
}

impl Future for FileFuture {
    type Item = Response;
    type Error = Error;
    fn poll(&mut self) -> Poll<Response, Error> {
        match File::open(&self.path) {
            Ok(mut file) => {
                let mut buf = Vec::new();
                match file.read_to_end(&mut buf) {
                    Ok(_) => {
                        let mime_type = file_path_mime(&self.path);
                        Ok(Async::Ready(
                            Response::new()
                                .with_status(StatusCode::Ok)
                                .with_header(ContentLength(buf.len() as u64))
                                .with_header(ContentType(mime_type))
                                .with_body(buf),
                        ))
                    }
                    Err(_) => Ok(Async::Ready(internal_server_error())),
                }
            }
            Err(e) => match e.kind() {
                io::ErrorKind::NotFound => Ok(Async::Ready(
                    Response::new().with_status(StatusCode::NotFound),
                )),
                _ => Ok(Async::Ready(internal_server_error())),
            },
        }
    }
}

fn file_path_mime(file_path: &Path) -> mime::Mime {
    let mime_type = match file_path.extension().and_then(std::ffi::OsStr::to_str) {
        Some("html") => mime::TEXT_HTML,
        Some("css") => mime::TEXT_CSS,
        Some("js") => mime::TEXT_JAVASCRIPT,
        Some("jpg") => mime::IMAGE_JPEG,
        Some("png") => mime::IMAGE_PNG,
        Some("wasm") => "application/wasm".parse::<mime::Mime>().unwrap(),
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

fn internal_server_error() -> Response {
    Response::new()
        .with_status(StatusCode::InternalServerError)
        .with_header(ContentLength(0))
}

// The custom Error type that encapsulates all the possible errors
// that can occur in this crate. This macro defines it and
// automatically creates Display, Error, and From implementations for
// all the variants.
error_type! {
    #[derive(Debug)]
    enum Error {
        Io(io::Error) { },
        AddrParse(std::net::AddrParseError) { },
        Std(Box<StdError + Send + Sync>) {
            desc (e) e.description();
        },
        ParseInt(std::num::ParseIntError) { },
    }
}
