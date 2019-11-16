//! Developer extensions for basic-http-server
//!
//! This code is not as clean and well-documented as main.rs,
//! but could still be a useful read.

use super::{Config, HtmlCfg};
use comrak::ComrakOptions;
use futures::{future, StreamExt};
use http::{Request, Response, StatusCode};
use hyper::{header, Body};
use log::{trace, warn};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use std::error::Error as StdError;
use std::ffi::OsStr;
use std::fmt::Write;
use std::io;
use std::path::{Path, PathBuf};
use tokio_fs::DirEntry;

/// The entry point to extensions. Extensions are given both the request and the
/// response result from regular file serving, and have the opportunity to
/// replace the response with their own response.
pub async fn serve(
    config: Config,
    req: Request<Body>,
    resp: super::Result<Response<Body>>,
) -> super::Result<Response<Body>> {
    trace!("checking extensions");

    if !config.use_extensions {
        return resp;
    }

    let path = super::local_path_for_request(&req.uri(), &config.root_dir)?;
    let file_ext = path.extension().and_then(OsStr::to_str).unwrap_or("");

    if file_ext == "md" {
        trace!("using markdown extension");
        return Ok(md_path_to_html(&path).await?);
    }

    match resp {
        Ok(mut resp) => {
            // Serve source code as plain text to render them in the browser
            maybe_convert_mime_type_to_text(&req, &mut resp);
            Ok(resp)
        }
        Err(super::Error::Io(e)) => {
            // If the requested file was not found, then try doing a directory listing.
            if e.kind() == io::ErrorKind::NotFound {
                let list_dir_resp = maybe_list_dir(&config.root_dir, &path).await?;
                trace!("using directory list extension");
                if let Some(f) = list_dir_resp {
                    Ok(f)
                } else {
                    Err(super::Error::from(e))
                }
            } else {
                Err(super::Error::from(e))
            }
        }
        r => r,
    }
}

/// Load a markdown file, render to HTML, and return the response.
async fn md_path_to_html(path: &Path) -> Result<Response<Body>> {
    // Render Markdown like GitHub
    let mut options = ComrakOptions::default();
    options.ext_autolink = true;
    options.ext_header_ids = None;
    options.ext_table = true;
    options.ext_strikethrough = true;
    options.ext_tagfilter = true;
    options.ext_tasklist = true;
    options.github_pre_lang = true;
    options.ext_header_ids = Some("user-content-".to_string());

    let buf = tokio::fs::read(path).await?;
    let s = String::from_utf8(buf).map_err(|_| Error::MarkdownUtf8)?;
    let html = comrak::markdown_to_html(&s, &options);
    let cfg = HtmlCfg {
        title: String::new(),
        body: html,
    };
    let html = super::render_html(cfg)?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, html.len() as u64)
        .header(header::CONTENT_TYPE, mime::TEXT_HTML.as_ref())
        .body(Body::from(html))
        .map_err(Error::from)
}

fn maybe_convert_mime_type_to_text(req: &Request<Body>, resp: &mut Response<Body>) {
    let path = req.uri().path();
    let file_name = path.rsplit('/').next();
    if let Some(file_name) = file_name {
        let mut do_convert = false;

        let ext = file_name.rsplit('.').next();
        if let Some(ext) = ext {
            if TEXT_EXTENSIONS.contains(&ext) {
                do_convert = true;
            }
        }

        if TEXT_FILES.contains(&file_name) {
            do_convert = true;
        }

        if do_convert {
            use http::header::HeaderValue;
            let val =
                HeaderValue::from_str(mime::TEXT_PLAIN.as_ref()).expect("mime is valid header");
            resp.headers_mut().insert(header::CONTENT_TYPE, val);
        }
    }
}

#[rustfmt::skip]
static TEXT_EXTENSIONS: &[&'static str] = &[
    "c",
    "cc",
    "cpp",
    "csv",
    "fst",
    "h",
    "java",
    "md",
    "mk",
    "proto",
    "py",
    "rb",
    "rs",
    "rst",
    "sh",
    "toml",
    "yml",
];

#[rustfmt::skip]
static TEXT_FILES: &[&'static str] = &[
    ".gitattributes",
    ".gitignore",
    ".mailmap",
    "AUTHORS",
    "CODE_OF_CONDUCT",
    "CONTRIBUTING",
    "COPYING",
    "COPYRIGHT",
    "Cargo.lock",
    "LICENSE",
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "Makefile",
    "rust-toolchain",
];

/// Try to treat the path as a directory and list the contents as HTML.
async fn maybe_list_dir(root_dir: &Path, path: &Path) -> Result<Option<Response<Body>>> {
    let meta = tokio::fs::metadata(path).await?;
    if meta.is_dir() {
        Ok(Some(list_dir(&root_dir, path).await?))
    } else {
        Ok(None)
    }
}

/// List the contents of a directory as HTML.
async fn list_dir(root_dir: &Path, path: &Path) -> Result<Response<Body>> {
    let up_dir = path.join("..");
    let path = path.to_owned();
    let dents = tokio::fs::read_dir(path).await?;
    let dents = dents.filter_map(|dent| match dent {
        Ok(dent) => future::ready(Some(dent)),
        Err(e) => {
            warn!("directory entry error: {}", e);
            future::ready(None)
        }
    });
    let paths = dents.map(|dent| DirEntry::path(&dent));
    let mut paths: Vec<_> = paths.collect().await;
    paths.sort();
    let paths = Some(up_dir).into_iter().chain(paths);
    let paths: Vec<_> = paths.collect();
    let html = make_dir_list_body(&root_dir, &paths)?;
    let resp = super::html_str_to_response(html, StatusCode::OK)?;
    Ok(resp)
}

fn make_dir_list_body(root_dir: &Path, paths: &[PathBuf]) -> Result<String> {
    let mut buf = String::new();

    writeln!(buf, "<div>").map_err(Error::WriteInDirList)?;

    let dot_dot = OsStr::new("..");

    for path in paths {
        let full_url = path
            .strip_prefix(root_dir)
            .map_err(Error::StripPrefixInDirList)?;
        let maybe_dot_dot = || {
            if path.ends_with("..") {
                Some(dot_dot)
            } else {
                None
            }
        };
        if let Some(file_name) = path.file_name().or_else(maybe_dot_dot) {
            if let Some(file_name) = file_name.to_str() {
                if let Some(full_url) = full_url.to_str() {
                    // %-encode filenames
                    // https://url.spec.whatwg.org/#fragment-percent-encode-set
                    const FRAGMENT_SET: &AsciiSet =
                        &CONTROLS.add(b' ').add(b'"').add(b'<').add(b'>').add(b'`');
                    const PATH_SET: &AsciiSet =
                        &FRAGMENT_SET.add(b'#').add(b'?').add(b'{').add(b'}');
                    let full_url = utf8_percent_encode(full_url, &PATH_SET);

                    // TODO: Make this a relative URL
                    writeln!(buf, "<div><a href='/{}'>{}</a></div>", full_url, file_name)
                        .map_err(Error::WriteInDirList)?;
                } else {
                    warn!("non-unicode url: {}", full_url.to_string_lossy());
                }
            } else {
                warn!("non-unicode path: {}", file_name.to_string_lossy());
            }
        } else {
            warn!("path without file name: {}", path.display());
        }
    }

    writeln!(buf, "</div>").map_err(Error::WriteInDirList)?;

    let cfg = HtmlCfg {
        title: String::new(),
        body: buf,
    };

    Ok(super::render_html(cfg)?)
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Display)]
pub enum Error {
    // blanket "pass-through" error types
    #[display(fmt = "engine error")]
    Engine(Box<super::Error>),

    #[display(fmt = "HTTP error")]
    Http(http::Error),

    #[display(fmt = "I/O error")]
    Io(io::Error),

    // custom "semantic" error types
    #[display(fmt = "markdown is not UTF-8")]
    MarkdownUtf8,

    #[display(fmt = "failed to strip prefix in directory listing")]
    StripPrefixInDirList(std::path::StripPrefixError),

    #[display(fmt = "formatting error while creating directory listing")]
    WriteInDirList(std::fmt::Error),
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        use Error::*;

        match self {
            Engine(e) => Some(e),
            Io(e) => Some(e),
            Http(e) => Some(e),
            MarkdownUtf8 => None,
            StripPrefixInDirList(e) => Some(e),
            WriteInDirList(e) => Some(e),
        }
    }
}

impl From<super::Error> for Error {
    fn from(e: super::Error) -> Error {
        Error::Engine(Box::new(e))
    }
}

impl From<http::Error> for Error {
    fn from(e: http::Error) -> Error {
        Error::Http(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}
