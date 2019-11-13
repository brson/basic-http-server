# `basic-http-server`

A simple static HTTP server, for learning and local development.

`basic-http-server` is designed for two purposes:

- _as a teaching tool_. It is a simple and well-commented example of
  basic [`tokio`], [`hyper`], and asynchronous Rust programming,
  with `async` / `await`.

- _for local development_. It serves static HTML content, and with the `-x`
   flag, provides convenience features useful for creating developer
   documentation, including markdown rendering and directory listing.
 
The entire reference source for setting up a `hyper` HTTP server is contained in
[`main.rs`]. The [`ext.rs`] file contains developer extensions.

[`tokio`]: https://github.com/tokio-rs/tokio
[`hyper`]: https://github.com/hyperium/hyper
[`main.rs`]: src/main.rs
[`ext.rs`]: src/ext.rs


## Developer extensions

When passed the `-x` flag, `basic-http-server` enables additional conveniences
useful for developing documentation locally. Those extensions are:

- Rendering files with the ".md" extension as Markdown.

- Listing directories when no "index.html" file is found.

This makes `basic-http-server` useful for the following scenarios:

- Previewing markdown content. Draft your `README.md` changes and view them
  locally before pushing to GitHub.

- Navigating to local documentation, including Rust API documentation. Just run
  `basic-http-server -x` in your project directory, and use the directory
  listing to navigate to `target/doc`, then find the crates to read from there
  (`cargo doc` doesn't put an `index.html` file in `target/doc`).


## Installation and Use

**Note that `basic-http-server` is not production-ready and should not be used
in production. It is a learning and development tool.**

Install with `cargo install`:

```sh
$ cargo install basic-http-server
$ basic-http-server
```

To turn on the developer extensions, pass `-x`:

```sh
$ basic-http-server -x
```

To increase logging verbosity use `RUST_LOG`:

```sh
RUST_LOG=basic_http_server=trace basic-http-server -x
```

Command line arguments:

```
USAGE:
        basic-http-server [FLAGS] [OPTIONS] [ARGS]

FLAGS:
    -x               Enable developer extensions
    -h, --help       Prints help information
    -V, --version    Prints version information

OPTIONS:
    -a, --addr <ADDR>    Sets the IP:PORT combination (default "127.0.0.1:4000")

ARGS:
    ROOT    Sets the root director (default ".")

```


## License

MIT/Apache-2.0
