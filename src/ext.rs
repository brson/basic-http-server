//! Developer extensions for basic-http-server

use super::{Config, HtmlCfg};
use super::{Error, Result};
use comrak::ComrakOptions;
use futures::{future, future::Either, Future, Stream};
use http::{Request, Response, StatusCode};
use hyper::{header, Body};
use std::ffi::OsStr;
use std::fmt::Write;
use std::io;
use std::path::{Path, PathBuf};
use tokio::fs::{self, File};
use tokio_fs::DirEntry;

pub fn serve(
    config: Config,
    req: Request<Body>,
    resp: super::Result<Response<Body>>,
) -> Box<dyn Future<Item = Response<Body>, Error = Error> + Send + 'static> {
    trace!("checking extensions");

    if !config.use_extensions {
        return Box::new(future::result(resp));
    }

    let path = super::local_path_for_request(&req.uri(), &config.root_dir);
    if config.single_page_app_mode {
        if let Some(ref path) = path {
            if !path.is_file() {
                let display_path = path.display().to_string();
                info!(
                    "replacing 404 or directory redirect for \"{}\" with index page",
                    &display_path[1..] // skip the initial '.'
                );
                let root_index = config.root_dir.join("index.html");
                if root_index.is_file() {
                    return Box::new(
                        File::open(root_index.clone())
                            .map_err(Error::from)
                            .and_then(move |file| super::respond_with_file(file, root_index)),
                    );
                }
            }
        }
    }
    if path.is_none() {
        return Box::new(future::result(resp));
    }
    let path = path.unwrap();
    let file_ext = path.extension().and_then(OsStr::to_str).unwrap_or("");

    if file_ext == "md" {
        trace!("using markdown extension");
        return Box::new(md_path_to_html(&path));
    }

    if let Err(e) = resp {
        match e {
            Error::Io(e) => {
                if e.kind() == io::ErrorKind::NotFound {
                    Box::new(maybe_list_dir(&config.root_dir, &path).and_then(
                        move |list_dir_resp| {
                            trace!("using directory list extension");
                            if let Some(f) = list_dir_resp {
                                Either::A(future::ok(f))
                            } else {
                                Either::B(future::err(Error::from(e)))
                            }
                        },
                    ))
                } else {
                    return Box::new(future::err(Error::from(e)));
                }
            }
            _ => {
                return Box::new(future::err(e));
            }
        }
    } else {
        Box::new(future::result(resp))
    }
}

fn md_path_to_html(path: &Path) -> impl Future<Item = Response<Body>, Error = Error> {
    File::open(path.to_owned()).then(move |open_result| match open_result {
        Ok(file) => Either::A(md_file_to_html(file)),
        Err(e) => Either::B(future::err(Error::Io(e))),
    })
}

fn md_file_to_html(file: File) -> impl Future<Item = Response<Body>, Error = Error> {
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

    super::read_file(file)
        .and_then(|s| String::from_utf8(s).map_err(|_| Error::MarkdownUtf8))
        .and_then(move |s: String| {
            let html = comrak::markdown_to_html(&s, &options);
            let cfg = HtmlCfg {
                title: String::new(),
                body: html,
            };
            super::render_html(cfg)
        })
        .and_then(move |html| {
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_LENGTH, html.len() as u64)
                .header(header::CONTENT_TYPE, mime::TEXT_HTML.as_ref())
                .body(Body::from(html))
                .map_err(Error::from)
        })
}

fn maybe_list_dir(
    root_dir: &Path,
    path: &Path,
) -> impl Future<Item = Option<Response<Body>>, Error = Error> {
    let root_dir = root_dir.to_owned();
    let path = path.to_owned();
    fs::metadata(path.clone())
        .map_err(Error::from)
        .and_then(move |m| {
            if m.is_dir() {
                Either::A(list_dir(&root_dir, &path))
            } else {
                Either::B(future::ok(None))
            }
        })
        .map_err(Error::from)
}

fn list_dir(
    root_dir: &Path,
    path: &Path,
) -> impl Future<Item = Option<Response<Body>>, Error = Error> {
    let root_dir = root_dir.to_owned();
    let up_dir = path.join("..");
    fs::read_dir(path.to_owned())
        .map_err(Error::from)
        .and_then(move |read_dir| {
            let root_dir = root_dir.to_owned();
            read_dir
                .collect()
                .map_err(Error::from)
                .and_then(move |dents| {
                    let paths = dents.iter().map(DirEntry::path);
                    let paths = Some(up_dir).into_iter().chain(paths);
                    let paths: Vec<_> = paths.collect();
                    make_dir_list_body(&root_dir, &paths).map_err(Error::from)
                })
                .and_then(|html| super::html_str_to_response(html, StatusCode::OK).map(Some))
        })
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
                // TODO: Make this a relative URL
                writeln!(
                    buf,
                    "<div><a href='/{}'>{}</a></div>",
                    full_url.display(),
                    file_name
                )
                .map_err(Error::WriteInDirList)?;
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
