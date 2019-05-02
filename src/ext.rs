use comrak::ComrakOptions;
use crate::Error;
use futures::{future, Future, future::Either, Stream};
use http::{Request, Response, StatusCode};
use hyper::{header, Body};
use std::path::{Path, PathBuf};
use std::ffi::OsStr;
use std::fmt::Write;
use super::{Config, HtmlCfg};
use tokio_fs::{self as fs, File, DirEntry};

pub fn serve(config: Config,
             req: Request<Body>,
             resp: Result<Response<Body>, Error>,
) -> Box<Future<Item = Response<Body>, Error = Error> + Send + 'static> {
    if !config.use_extensions {
        return Box::new(future::result(resp));
    }
    
    let path = super::local_path_for_request(&req, &config.root_dir);
    if path.is_none() { return Box::new(future::result(resp)); }
    let path = path.unwrap();

    let file_ext = path.extension().and_then(OsStr::to_str).unwrap_or("");

    if file_ext == "md" {
        return Box::new(md_path_to_html(&path));
    }

    if let Ok(resp) = resp {
        if resp.status() != StatusCode::NOT_FOUND {
            return Box::new(future::ok(resp));
        }

        Box::new(maybe_list_dir(&path).and_then(move |list_dir_resp| {
            if let Some(f) = list_dir_resp {
                Either::A(future::ok(f))
            } else {
                Either::B(future::ok(resp))
            }
        }))
    } else {
        Box::new(future::result(resp))
    }
}

fn md_path_to_html(path: &Path)
                   -> impl Future<Item = Response<Body>, Error = Error>
{
    File::open(path.to_owned()).then(
        move |open_result| match open_result {
            Ok(file) => Either::A(md_file_to_html(file)),
            Err(e) => Either::B(future::err(Error::Io(e))),
        }
    )
}

fn md_file_to_html(file: File)
                   -> impl Future<Item = Response<Body>, Error = Error>
{
    let mut options = ComrakOptions::default();
    options.ext_autolink = true;
    options.ext_table = true;
    
    super::read_file(file)
        .and_then(|s| String::from_utf8(s).map_err(|_| Error::MarkdownUtf8(true)))
        .and_then(move |s: String| {
            let html = comrak::markdown_to_html(&s, &options);
            let cfg = HtmlCfg {
                title: String::new(),
                body: html,
            };
            super::render_html(cfg)
        }).and_then(move |html| {
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_LENGTH, html.len() as u64)
                .header(header::CONTENT_TYPE, mime::TEXT_HTML.as_ref())
                .body(Body::from(html))
                .map_err(Error::from)
        })
}

fn maybe_list_dir(path: &Path)
                  -> impl Future<Item = Option<Response<Body>>, Error = Error>
{
    let path = path.to_owned();
    fs::metadata(path.clone()).map_err(Error::from).and_then(move |m| {
        if m.is_dir() {
            Either::A(list_dir(&path))
        } else {
            Either::B(future::ok(None))
        }
    }).map_err(Error::from)
}

fn list_dir(path: &Path)
            -> impl Future<Item = Option<Response<Body>>, Error = Error>
{
    fs::read_dir(path.to_owned()).map_err(Error::from).and_then(|read_dir| {
        read_dir.collect().map_err(Error::from).and_then(|dents| {
            let paths: Vec<_> = dents.iter().map(DirEntry::path).collect();
            make_dir_list_body(&paths).map_err(Error::from)
        }).and_then(|html| {
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_LENGTH, html.len() as u64)
                .header(header::CONTENT_TYPE, mime::TEXT_HTML.as_ref())
                .body(Body::from(html))
                .map_err(Error::from)
                .map(Some)
        })
    })
}

fn make_dir_list_body(paths: &[PathBuf]) -> Result<String, Error> {
    let mut buf = String::new();

    writeln!(buf, "<div>")?;

    for path in paths {
        //let link = path_to_link(path);
        writeln!(buf,
                 "<span><a href='{}'>{}</a></span>",
                 "todo",
                 path.display())?;
    }

    writeln!(buf, "</div>")?;

    let cfg = HtmlCfg {
        title: String::new(),
        body: buf,
    };
    super::render_html(cfg)
}
