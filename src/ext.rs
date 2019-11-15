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
use std::ffi::OsStr;
use std::fmt::Write;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;
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

    let path = super::local_path_for_request(&req.uri(), &config.root_dir);
    let path = if let Some(path) = path {
        path
    } else {
        return resp;
    };
    let file_ext = path.extension().and_then(OsStr::to_str).unwrap_or("");

    if file_ext == "md" {
        trace!("using markdown extension");
        return Ok(md_path_to_html(&path).await?);
    }

    // If the requested file was not found, then try doing a directory listing.
    if let Err(e) = resp {
        match e {
            super::Error::Io { source: e } => {
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
            _ => Err(e),
        }
    } else {
        resp
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
    let paths: Vec<_> = paths.collect().await;
    let paths = Some(up_dir).into_iter().chain(paths);
    let paths: Vec<_> = paths.collect();
    let html = make_dir_list_body(&root_dir, &paths)?;
    let resp = super::html_str_to_response(html, StatusCode::OK)?;
    Ok(resp)
}

fn make_dir_list_body(root_dir: &Path, paths: &[PathBuf]) -> Result<String> {
    let mut buf = String::new();

    writeln!(buf, "<div>").map_err(|source| Error::WriteInDirList { source })?;

    let dot_dot = OsStr::new("..");

    for path in paths {
        let full_url = path
            .strip_prefix(root_dir)
            .map_err(|source| Error::StripPrefixInDirList { source })?;
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
                        .map_err(|source| Error::WriteInDirList { source })?;
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

    writeln!(buf, "</div>").map_err(|source| Error::WriteInDirList { source })?;

    let cfg = HtmlCfg {
        title: String::new(),
        body: buf,
    };

    Ok(super::render_html(cfg)?)
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    // blanket "pass-through" error types
    #[error("engine error")]
    Engine { source: Box<super::Error> },

    #[error("HTTP error")]
    Http {
        #[from]
        source: http::Error,
    },

    #[error("I/O error")]
    Io {
        #[from]
        source: io::Error,
    },

    // custom "semantic" error types
    #[error("markdown is not UTF-8")]
    MarkdownUtf8,

    #[error("failed to strip prefix in directory listing")]
    StripPrefixInDirList { source: std::path::StripPrefixError },

    #[error("formatting error while creating directory listing")]
    WriteInDirList { source: std::fmt::Error },
}

impl From<super::Error> for Error {
    fn from(e: super::Error) -> Error {
        Error::Engine {
            source: Box::new(e),
        }
    }
}
