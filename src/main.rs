extern crate clap;
extern crate rotor;
extern crate rotor_stream;
extern crate rotor_http;
extern crate mio;
extern crate time;
extern crate threadpool;
extern crate num_cpus;
extern crate net2;
extern crate libc;

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
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use time::{Duration, SteadyTime};
use threadpool::ThreadPool;

fn main() {
    if let Err(e) = run() {
        println!("error: {}", e.description());
    }
}

fn run() -> Result<(), Error> {
    let matches = App::new("basic-http-server")
        .version("0.1")
        .about("A basic HTTP file server")
        .args_from_usage(
            "[ROOT] 'Sets the root dir (default \".\")'
             -a --addr=[ADDR] 'Sets the IP:PORT combination (default \"127.0.0.1:4000\")'
             -t --threads=[THREADS] 'Sets the number of server threads (default 4)'")
        .get_matches();

    let root_dir = matches.value_of("ROOT").unwrap_or(".");
    let addr = matches.value_of("ADDR").unwrap_or("127.0.0.1:4000");
    let num_rotor_threads = match matches.value_of("THREADS") {
        Some(t) => { try!(t.parse()) }
        None => 4
    };

    let root_dir = PathBuf::from(root_dir);
    let addr: SocketAddr = try!(addr.parse());

    let thread_pool = ThreadPool::new(400);

    for _ in 0..num_rotor_threads {
        let root_dir = root_dir.clone();
        let addr = addr.clone();
        let thread_pool = thread_pool.clone();
        thread::spawn(move || {
            // Our custom server context
            let context = ServerContext {
                root_dir: root_dir,
                thread_pool: thread_pool,
            };

            let sock = net2::TcpBuilder::new_v4().unwrap();
            set_reuse_port(&sock);
            sock.bind(&addr).unwrap();

            let listener = sock.listen(4096).unwrap();
            let listener = TcpListener::from_listener(listener, &addr).unwrap();

            // The mio event loop
            let mut event_loop = rotor::EventLoop::new().unwrap();
            // Rotor's mio event loop handler
            let mut handler = rotor::Handler::new(context, &mut event_loop);

            handler.add_machine_with(&mut event_loop, |scope| {
                Accept::<Stream<Parser<ServerState, _>>, _>::new(listener, scope)
            }).unwrap();

            println!("listening on {}", addr);

            event_loop.run(&mut handler).unwrap();
        });
   }

    let (_tx, rx) = mpsc::channel::<()>();
    rx.recv().unwrap();

    Ok(())
}

struct ServerContext {
    root_dir: PathBuf,
    thread_pool: ThreadPool
}

impl Context for ServerContext { }

enum ServerState {
    Init,
    Ready(Head),
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

impl Server for ServerState {
    type Context = ServerContext;

    fn headers_received(_head: &Head, _scope: &mut Scope<Self::Context>)
                        -> Result<(Self, RecvMode, Deadline), StatusCode> {
        Ok((ServerState::Init, RecvMode::Buffered(1024),
            Deadline::now() + Duration::seconds(10)))
    }

    fn request_start(self, head: Head, _response: &mut Response,
                     _scope: &mut Scope<Self::Context>)
                     -> Option<Self> {
        Some(ServerState::Ready(head))
    }

    fn request_received(self, _data: &[u8], response: &mut Response,
                        scope: &mut Scope<Self::Context>)
                        -> Option<Self> {

        let head = if let ServerState::Ready(head) = self {
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
        // This rotor Notifier will trigger a wakeup.
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

        Some(ServerState::WaitingForData(rx, false))
    }

    fn wakeup(self, response: &mut Response, _scope: &mut Scope<Self::Context>)
              -> Option<Self> {
        let mut state = self;
        loop {
            state = match state {
                ServerState::WaitingForData(rx, headers_sent) => {
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
                            ServerState::WaitingForData(rx, true)
                        }
                        Ok(DataMsg::Data(buf)) => {
                            assert!(headers_sent);
                            response.write_body(&buf);
                            ServerState::WaitingForData(rx, headers_sent)
                        }
                        Ok(DataMsg::Done) => {
                            assert!(headers_sent);
                            response.done();
                            return None;
                        }
                        Ok(DataMsg::IoError(_)) => {
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
                        Err(mpsc::TryRecvError::Empty) => {
                            return Some(ServerState::WaitingForData(rx, headers_sent));
                        }
                        Err(mpsc::TryRecvError::Disconnected) => {
                            unreachable!()
                        }
                    }
                }
                _ => {
                    unreachable!()
                }
            }
        }
    }

    fn request_chunk(self, _chunk: &[u8], _response: &mut Response,
                     _scope: &mut Scope<Self::Context>)
                     -> Option<Self> {
        Some(self)
    }

    fn request_end(self, _response: &mut Response,
                   _scope: &mut Scope<Self::Context>)
                   -> Option<Self> {
        Some(self)
    }

    fn timeout(self, _response: &mut Response, _scope: &mut Scope<Self::Context>)
               -> Option<(Self, Deadline)> {
        Some((self, SteadyTime::now()))
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

#[derive(Debug)]
enum Error {
    IoError(io::Error),
    AddrParseError(std::net::AddrParseError),
    StdError(Box<StdError>),
    ParseIntError(std::num::ParseIntError),
}

impl StdError for Error {
    fn description(&self) -> &str {
        match *self {
            Error::IoError(ref e) => e.description(),
            Error::AddrParseError(ref e) => e.description(),
            Error::StdError(ref e) => e.description(),
            Error::ParseIntError(ref e) => e.description(),
        }
    }
    fn cause(&self) -> Option<&StdError> {
        match *self {
            Error::IoError(ref e) => Some(e),
            Error::AddrParseError(ref e) => Some(e),
            Error::StdError(ref e) => Some(&**e),
            Error::ParseIntError(ref e) => Some(e),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        match *self {
            Error::IoError(ref e) => e.fmt(fmt),
            Error::AddrParseError(ref e) => e.fmt(fmt),
            Error::StdError(ref e) => e.fmt(fmt),
            Error::ParseIntError(ref e) => e.fmt(fmt),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::IoError(e)
    }
}

impl From<std::net::AddrParseError> for Error {
    fn from(e: std::net::AddrParseError) -> Error {
        Error::AddrParseError(e)
    }
}

impl From<Box<StdError>> for Error {
    fn from(e: Box<StdError>) -> Error {
        Error::StdError(e)
    }
}

impl From<std::num::ParseIntError> for Error {
    fn from(e: std::num::ParseIntError) -> Error {
        Error::ParseIntError(e)
    }
}
