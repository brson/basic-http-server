//! Developer extensions for basic-http-server

use super::{Config, HtmlCfg};
use super::{Error, Result};
use comrak::ComrakOptions;
use futures::StreamExt;
use http::{Request, Response, StatusCode};
use hyper::{header, Body};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use std::ffi::OsStr;
use std::fmt::Write;
use std::io;
use std::path::{Path, PathBuf};
use tokio_fs::DirEntry;

pub async fn serve(
    config: Config,
    req: Request<Body>,
    resp: super::Result<Response<Body>>,
) -> Result<Response<Body>> {
    trace!("checking extensions");

    if !config.use_extensions {
        return resp;
    }

    let path = super::local_path_for_request(&req.uri(), &config.root_dir);
    if path.is_none() {
        return resp;
    }
    let path = path.unwrap();
    let file_ext = path.extension().and_then(OsStr::to_str).unwrap_or("");

    if file_ext == "md" {
        trace!("using markdown extension");
        return md_path_to_html(&path).await;
    }

    if let Err(e) = resp {
        match e {
            Error::Io(e) => {
                if e.kind() == io::ErrorKind::NotFound {
                    let list_dir_resp = maybe_list_dir(&config.root_dir, &path).await?;
                    trace!("using directory list extension");
                    if let Some(f) = list_dir_resp {
                        Ok(f)
                    } else {
                        Err(Error::from(e))
                    }
                } else {
                    Err(Error::from(e))
                }
            }
            _ => {
                Err(Error::from(e))
            }
        }
    } else {
        resp
    }
}

async fn md_path_to_html(path: &Path) -> Result<Response<Body>> {
    let mut options = ComrakOptions::default();
    // be like GitHub
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

async fn maybe_list_dir(
    root_dir: &Path,
    path: &Path,
) -> Result<Option<Response<Body>>> {
    let meta = tokio::fs::metadata(path).await?;
    if meta.is_dir() {
        list_dir(&root_dir, path).await
    } else {
        Ok(None)
    }
}

// FIXME: This doesn't make use of the Option return
async fn list_dir(
    root_dir: &Path,
    path: &Path,
) -> Result<Option<Response<Body>>> {
    let up_dir = path.join("..");
    let path = path.to_owned();
    let dents = tokio::fs::read_dir(path).await?;
    let dents: Vec<_> = dents.collect().await;
    let dents: Vec<_> = dents.into_iter().filter_map(|dent| {
        match dent {
            Ok(dent) => Some(dent),
            Err(e) => {
                warn!("directory entry error: {}", e);
                None
            }
        }
    }).collect();
    let paths = dents.iter().map(DirEntry::path);
    let paths = Some(up_dir).into_iter().chain(paths);
    let paths: Vec<_> = paths.collect();
    let html = make_dir_list_body(&root_dir, &paths)?;
    let resp = super::html_str_to_response(html, StatusCode::OK).map(Some)?;
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
                    const FRAGMENT_SET: &AsciiSet = &CONTROLS.add(b' ').add(b'"').add(b'<').add(b'>').add(b'`');
                    const PATH_SET: &AsciiSet = &FRAGMENT_SET.add(b'#').add(b'?').add(b'{').add(b'}');
                    let full_url = utf8_percent_encode(full_url, &PATH_SET);

                    // TODO: Make this a relative URL
                    writeln!(
                        buf,
                        "<div><a href='/{}'>{}</a></div>",
                        full_url,
                        file_name
                    )
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
    super::render_html(cfg)
}
