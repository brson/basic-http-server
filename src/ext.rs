use comrak::ComrakOptions;
use crate::Error;
use futures::{future, Future, future::Either};
use http::{Request, Response, StatusCode};
use hyper::{header, Body};
use std::path::Path;
use std::ffi::OsStr;
use super::HtmlCfg;
use tokio::fs::File;

pub fn map(req: &Request<Body>,
           resp: Response<Body>,
           root_dir: &Path,
           ext: bool,
) -> Box<Future<Item = Response<Body>, Error = Error> + Send + 'static> {
    if !ext {
        return Box::new(future::ok(resp));
    }
    
    let path = super::local_path_for_request(req, root_dir);
    if path.is_none() { return Box::new(future::ok(resp)); }
    let path = path.unwrap();

    let file_ext = path.extension().and_then(OsStr::to_str).unwrap_or("");

    if file_ext == "md" {
        return Box::new(md_path_to_html(&path));
    }

    if resp.status() != StatusCode::NOT_FOUND {
        return Box::new(future::ok(resp));
    }

    Box::new(future::ok(resp))
}

fn md_path_to_html(path: &Path)
                   -> impl Future<Item = Response<Body>, Error = Error>
{
    File::open(path.to_owned()).then(
        move |open_result| match open_result {
            Ok(file) => Either::A(md_file_to_html(file)),
            Err(e) => Either::B(super::handle_io_error(e)),
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
