# `basic-http-server`

A simple static HTTP server, for learning and local development.

`basic-http-server` is designed for two purposes:

- _as a teaching tool_. It is a simple and well-commented example of
  basic [`axum`], [`tower`], [`tokio`], and asynchronous Rust programming,
  with `async` / `await`.

- _for local development_. It serves static HTML content, and with the `-x`
   flag, provides convenience features useful for creating developer
   documentation, including markdown rendering with syntax highlighting,
   and directory listing.

The core server setup is contained in [`main.rs`]. Error types and HTML
rendering are in [`server.rs`]. The developer extensions are in [`ext/`],
with each extension implemented as tower middleware.

[`axum`]: https://github.com/tokio-rs/axum
[`tower`]: https://github.com/tower-rs/tower
[`tokio`]: https://github.com/tokio-rs/tokio
[`main.rs`]: src/main.rs
[`server.rs`]: src/server.rs
[`ext/`]: src/ext/


## Developer extensions

When passed the `-x` flag, `basic-http-server` enables additional conveniences
useful for developing documentation locally. Those extensions are:

- Rendering files with the ".md" extension as Markdown, with syntax
  highlighting for fenced code blocks via [syntect]. Supports Rust, Python,
  Java, Bash, Go, C, JavaScript, TypeScript, and many other languages.

- Listing directories when no "index.html" file is found.

- Serving common source code files as "text/plain" so they are
  rendered in the browser.

All rendered pages support dark and light mode, following the
system preference.

[syntect]: https://github.com/trishume/syntect

This makes `basic-http-server` useful for the following scenarios:

- Previewing markdown content. Draft your `README.md` changes and view them
  locally before pushing to GitHub.

- Navigating to local documentation, including Rust API documentation. Just run
  `basic-http-server -x` in your project directory, and use the directory
  listing to navigate to `target/doc`, then find the crates to read from there
  (`cargo doc` doesn't put an `index.html` file in `target/doc`).


## Installation and Use

**Note that `basic-http-server` is not production-ready and should not be
exposed to the internet. It is a learning and development tool.**

Install with `cargo install`:

```sh
$ cargo install basic-http-server
$ basic-http-server
```

To turn on the developer extensions, pass `-x`:

```sh
$ basic-http-server -x
```

Set a custom port with `-p`:

```sh
$ basic-http-server -x -p8080
```

Listen on all interfaces with `--public`:

```sh
$ basic-http-server -x -p8080 --public
```

To increase logging verbosity use `RUST_LOG`:

```sh
RUST_LOG=basic_http_server=trace basic-http-server -x
```

Command line arguments:

```
Usage: basic-http-server [OPTIONS] [ROOT_DIR]

Arguments:
  [ROOT_DIR]  The root directory for serving files [default: .]

Options:
  -a, --addr <ADDR>  The IP:PORT combination
  -p, --port <PORT>  Port number [default: 4000]
      --public       Listen on all interfaces (0.0.0.0) instead of localhost
  -x                 Enable developer extensions
  -h, --help         Print help
  -V, --version      Print version
```


## License

MIT/Apache-2.0
