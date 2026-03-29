mod normal;
mod extensions;

use std::io::Read;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// A running server process with an HTTP client.
pub struct TestServer {
    child: Child,
    pub addr: SocketAddr,
    pub client: reqwest::Client,
}

impl TestServer {
    /// Start the server binary serving the given root directory.
    ///
    /// Binds to port 0 and parses the actual address from log output.
    pub fn start(root: &Path, extensions: bool) -> Self {
        let binary = std::env::var("BHS_BINARY")
            .unwrap_or_else(|_| env!("CARGO_BIN_EXE_basic-http-server").to_string());

        let mut cmd = Command::new(&binary);
        cmd.arg("--addr").arg("127.0.0.1:0");
        cmd.arg(root);
        if extensions {
            cmd.arg("-x");
        }
        cmd.env("RUST_LOG", "basic_http_server=info");
        cmd.stderr(Stdio::piped());
        cmd.stdout(Stdio::null());

        let mut child = cmd.spawn().unwrap_or_else(|e| {
            panic!("failed to start server binary '{}': {}", binary, e);
        });

        // Parse the bound address from stderr log output.
        // Read stderr byte-by-byte into a buffer to avoid blocking on BufReader
        // after all startup lines have been read.
        let mut stderr = child.stderr.take().unwrap();
        let mut addr = None;
        let mut buf = Vec::new();

        let mut byte = [0u8; 1];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        while std::time::Instant::now() < deadline {
            match stderr.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    buf.push(byte[0]);
                    if byte[0] == b'\n' {
                        let line = String::from_utf8_lossy(&buf);
                        let clean = strip_ansi(&line);
                        if let Some(pos) = clean.find("listening on ") {
                            let addr_str = &clean[pos + "listening on ".len()..];
                            if let Ok(a) = addr_str.trim().parse::<SocketAddr>() {
                                addr = Some(a);
                                break;
                            }
                        }
                        buf.clear();
                    }
                }
                Err(e) => panic!("failed to read server stderr: {}", e),
            }
        }

        let addr = addr.expect("failed to parse server address from log output");
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        TestServer {
            child,
            addr,
            client,
        }
    }

    /// GET a path and return the response.
    pub async fn get(&self, path: &str) -> reqwest::Response {
        let url = format!("http://{}{}", self.addr, path);
        self.client.get(&url).send().await.unwrap()
    }

    /// Send a request with an arbitrary method.
    pub async fn request(&self, method: reqwest::Method, path: &str) -> reqwest::Response {
        let url = format!("http://{}{}", self.addr, path);
        self.client.request(method, &url).send().await.unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Path to the test fixtures directory.
pub fn fixtures_dir() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p
}

/// Strip ANSI escape sequences from a string.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip until we hit a letter (the terminator of the escape sequence).
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
