/*

A simple HTTP server that serves static content from a given directory,
built on [rotor] and [rotor-http].

It creates a number of rotor server threads, all listening on the same
port (via [libc::SO_REUSEPORT]). These are state machines performing
non-blocking network I/O on top of [mio]. The HTTP requests are parsed
and responses emitted on these threads.

Files are read sequentially in a thread pool. You might think they
would be read on the I/O loop, but no: async file I/O is hard, and mio
is only for network I/O.

[rotor]: https://github.com/tailhook/rotor
[rotor-http]: https://github.com/tailhook/rotor-http
[libc::SO_REUSEPORT]: https://lwn.net/Articles/542629/
[mio]: https://github.com/carllerche/mio

*/

// Non-blocking I/O.
//
// https://github.com/carllerche/mio
extern crate mio;

// rotor, a library for building state machines on top of mio, along
// with an HTTP implementation, and its stream abstraction.
//
// https://medium.com/@paulcolomiets/async-io-in-rust-part-iii-cbfd10f17203
extern crate rotor;
extern crate rotor_http;
extern crate rotor_stream;

// A simple library for dealing with command line arguments
//
// https://github.com/kbknapp/clap-rs
extern crate clap;

// A basic thread pool.
//
// http://frewsxcv.github.io/rust-threadpool/threadpool/
extern crate threadpool;

// Extensions to the standard networking types.
//
// This is an official nursery crate that contains networking features
// that aren't in std. We're using in for [TcpBuilder].

// https://doc.rust-lang.org/net2-rs/net2/index.html
//
// [TcpBuilder]: https://doc.rust-lang.org/net2-rs/net2/struct.TcpBuilder.html
extern crate net2;

// Bindings to the C library.
//
// We need it for `setsockopt` and `SO_REUSEPORT`.
//
// http://doc.rust-lang.org/libc/index.html
extern crate libc;

// The error_type! macro to avoid boilerplate trait
// impls for error handling.
#[macro_use]
extern crate error_type;

// Some deprecated time types that rotor needs, Duration,
// and SteadyTime, that rotor needs.
extern crate time;

use clap::App;
use mio::tcp::TcpListener;
use rotor::Scope;
use rotor_http::header::ContentLength;
use rotor_http::server::{RecvMode, Server, Head, Response, Parser, Context};
use rotor_http::status::StatusCode;
use rotor_http::uri::RequestUri;
use rotor_stream::{Deadline, Accept, Stream};
use std::error::Error as StdError;
use std::fs::File;
use std::io::{self, Read};
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use threadpool::ThreadPool;
use time::Duration;

fn main() {
    // Set up our error handling immediatly. Everything in this crate
    // that can return an error returns our custom Error type. `try!`
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
    let config = try!(parse_config_from_cmdline());

    let (tx, rx) = mpsc::channel::<Result<(), Error>>();

    // Create multiple threads all listening on the same address and
    // port, and sharing a thread pool for their file I/O.
    // TODO: This needs to report panicks.
    for _ in 0..config.num_server_threads {
        let tx = tx.clone();
        let config = config.clone();

        thread::spawn(move || {
            let r = run_server(config);

            // It would be very strange for this send to fail,
            // but there's nothing we can do if it does.
            tx.send(r).unwrap();
        });
    }

    // Wait for each thread to exit and report the result. Note that
    // there's no way for the server threads to exit successfully,
    // so normally this will block forever.
    for i in 0..config.num_server_threads {
        match rx.recv() {
            Ok(Ok(())) => {
                println!("thread {} exited successfully", i);
            }
            Ok(Err(e)) => {
                println!("thread {} exited with error: {}", i, e.description());
            }
            Err(e) => {
                // This will happen if some threads panicked.
                println!("thread {} disappeared: {:?}", i, e.description());
            }
        }
    }

    Ok(())
}

// The configuration object, created from command line options
#[derive(Clone)]
struct Config {
    addr: SocketAddr,
    root_dir: PathBuf,
    thread_pool: ThreadPool,
    num_server_threads: u16,
}

fn parse_config_from_cmdline() -> Result<Config, Error> {
    let matches = App::new("basic-http-server")
        .version("0.1")
        .about("A basic HTTP file server")
        .args_from_usage(
            "[ROOT] 'Sets the root dir (default \".\")'
             -a --addr=[ADDR] 'Sets the IP:PORT combination (default \"127.0.0.1:4000\")'
             -t --threads=[THREADS] 'Sets the number of server threads (default 4)'
             --file-threads=[FILE-THREADS] 'Sets the number of threads in the file I/O thread pool (default 100)'")
        .get_matches();

    let default_server_threads = 4;
    let default_file_threads = 100;

    let addr = matches.value_of("ADDR").unwrap_or("127.0.0.1:4000");
    let root_dir = matches.value_of("ROOT").unwrap_or(".");
    let num_server_threads = match matches.value_of("THREADS") {
        Some(t) => { try!(t.parse()) }
        None => default_server_threads
    };
    let num_file_threads = match matches.value_of("FILE-THREADS") {
        Some(t) => { try!(t.parse()) }
        None => default_file_threads
    };

    // Display the configuration to be helpful
    println!("addr: {}", addr);
    println!("root dir: {:?}", root_dir);
    println!("server threads: {}", num_server_threads);
    println!("file threads: {}", num_file_threads);
    println!("");

    Ok(Config {
        addr: try!(addr.parse()),
        root_dir: PathBuf::from(root_dir),
        thread_pool: ThreadPool::new(num_file_threads),
        num_server_threads: num_server_threads,
    })
}

// Run a single HTTP server forever.
fn run_server(config: Config) -> Result<(), Error> {
    let Config {
        addr, root_dir, thread_pool, ..
    } = config;

    // Our custom server context
    let context = ServerContext {
        root_dir: root_dir,
        thread_pool: thread_pool,
    };

    let sock = try!(net2::TcpBuilder::new_v4());
    set_reuse_port(&sock);
    try!(sock.bind(&addr));

    let listener = try!(sock.listen(4096));
    let listener = try!(TcpListener::from_listener(listener, &addr));

    let config = rotor::Config::new();
    let event_loop = try!(rotor::Loop::new(&config));
    let mut loop_inst = event_loop.instantiate(context);

    loop_inst.add_machine_with(|scope| {
        Accept::<Stream<Parser<RequestState, _>>, _>::new(listener, scope)
    }).unwrap();

    println!("listening on {}", addr);

    try!(loop_inst.run());

    Ok(())
}

// The ServerContext, implementing the rotor-http Context,
// and RequestState, implementing the rotor-http Server.
//
// RequestState is a state machine that lasts for the lifecycle of a
// single request. All RequestStates have access to the shared
// ServerContext.

struct ServerContext {
    root_dir: PathBuf,
    thread_pool: ThreadPool
}

impl Context for ServerContext { }

enum RequestState {
    Init,
    ReadyToRespond(Head),
    WaitingForData(Receiver<DataMsg>, bool /* headers_sent */)
}

// Messages sent from the file I/O thread back to the state machine
enum DataMsg {
    NotFound,
    Header(u64),
    Data(Vec<u8>),
    Done,
    IoError(std::io::Error),
}

impl Server for RequestState {
    type Context = ServerContext;

    fn headers_received(_head: &Head, _scope: &mut Scope<Self::Context>)
                        -> Result<(Self, RecvMode, Deadline), StatusCode> {
        Ok((RequestState::Init, RecvMode::Buffered(1024),
            Deadline::now() + Duration::seconds(10)))
    }

    fn request_start(self, head: Head, _response: &mut Response,
                     _scope: &mut Scope<Self::Context>)
                     -> Option<Self> {
        Some(RequestState::ReadyToRespond(head))
    }

    fn request_received(self, _data: &[u8], response: &mut Response,
                        scope: &mut Scope<Self::Context>)
                        -> Option<Self> {

        // Now that the request is received, prepare the response.

        let head = if let RequestState::ReadyToRespond(head) = self {
            head
        } else {
            unreachable!()
        };

        let path = if let Some(path) = local_path_for_request(head, &scope.root_dir) {
            path
        } else {
            internal_server_error(response);
            return None;
        };

        // We're going to do the file I/O in another thread.
        // This channel will transmit info from the I/O thread to the
        // rotor machine.
        let (tx, rx) = mpsc::channel();
        // This rotor Notifier will trigger a wakeup when data is
        // ready, upon which the response will be written in `wakeup`.
        let notifier = scope.notifier();

        scope.thread_pool.execute(move || {
            match File::open(path) {
                Ok(mut file) => {
                    let mut buf = Vec::new();
                    match file.read_to_end(&mut buf) {
                        Ok(_) => {
                            tx.send(DataMsg::Header(buf.len() as u64)).unwrap();
                            tx.send(DataMsg::Data(buf)).unwrap();
                            tx.send(DataMsg::Done).unwrap();
                        }
                        Err(e) => {
                            tx.send(DataMsg::IoError(e)).unwrap();
                        }
                    }
                }
                Err(e) => {
                    match e.kind() {
                        io::ErrorKind::NotFound => {
                            tx.send(DataMsg::NotFound).unwrap();
                        }
                        _ => {
                            tx.send(DataMsg::IoError(e)).unwrap();
                        }
                    }
                }
            }

            notifier.wakeup().unwrap();
        });

        Some(RequestState::WaitingForData(rx, false))
    }

    fn wakeup(self, response: &mut Response, _scope: &mut Scope<Self::Context>)
              -> Option<Self> {

        // Write the HTTP response in reaction to the messages sent by
        // the file I/O thread.

        let mut state = self;
        loop {
            state = match state {
                RequestState::WaitingForData(rx, headers_sent) => {
                    match rx.try_recv() {
                        Ok(DataMsg::NotFound) => {
                            response.status(StatusCode::NotFound);
                            response.add_header(ContentLength(0)).unwrap();
                            response.done_headers().unwrap();
                            response.done();
                            return None;
                        }
                        Ok(DataMsg::Header(length)) => {
                            response.status(StatusCode::Ok);
                            response.add_header(ContentLength(length)).unwrap();
                            response.done_headers().unwrap();
                            RequestState::WaitingForData(rx, true)
                        }
                        Ok(DataMsg::Data(buf)) => {
                            assert!(headers_sent);
                            response.write_body(&buf);
                            RequestState::WaitingForData(rx, headers_sent)
                        }
                        Ok(DataMsg::Done) => {
                            assert!(headers_sent);
                            response.done();
                            return None;
                        }
                        Err(mpsc::TryRecvError::Empty) => {
                            return Some(RequestState::WaitingForData(rx, headers_sent));
                        }
                        Ok(DataMsg::IoError(_)) |
                        Err(mpsc::TryRecvError::Disconnected) => {
                            if headers_sent {
                                // We've arleady said this isn't an
                                // error by sending successful
                                // headers. Just give up.
                                response.done();
                                return None;
                            } else {
                                internal_server_error(response);
                                return None;
                            }
                        }
                    }
                }
                _ => {
                    unreachable!()
                }
            }
        }
    }

    // I don't know what to do with these yet.

    fn request_chunk(self, _chunk: &[u8], _response: &mut Response,
                     _scope: &mut Scope<Self::Context>)
                     -> Option<Self> {
        unimplemented!()
    }

    fn request_end(self, _response: &mut Response,
                   _scope: &mut Scope<Self::Context>)
                   -> Option<Self> {
        unimplemented!()
    }

    fn timeout(self, _response: &mut Response, _scope: &mut Scope<Self::Context>)
               -> Option<(Self, Deadline)> {
        unimplemented!()
    }

}

fn local_path_for_request(head: Head, root_dir: &Path) -> Option<PathBuf> {
    let request_path = match head.uri {
        RequestUri::AbsolutePath(p) => p,
        _ => {
            return None;
        }
    };

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

fn set_reuse_port(sock: &net2::TcpBuilder) {
    let one = 1i32;
    unsafe {
        assert!(libc::setsockopt(
            sock.as_raw_fd(), libc::SOL_SOCKET,
            libc::SO_REUSEPORT,
            &one as *const libc::c_int as *const libc::c_void, 4) == 0);
    }
}

fn internal_server_error(response: &mut Response) {
    response.status(StatusCode::InternalServerError);
    response.add_header(ContentLength(0)).unwrap();
    response.done_headers().unwrap();
    response.done();
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
