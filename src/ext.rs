use crate::Error;
use futures::{future, Future};
use http::{Request, Response, StatusCode};
use hyper::Body;
use std::path::{Path, PathBuf};
use std::ffi::OsStr;

pub fn map(req: &Request<Body>,
           resp: Response<Body>,
           root_dir: &Path,
           ext: bool,
) -> Box<Future<Item = Response<Body>, Error = Error> + Send + 'static> {
    if !ext || resp.status() != StatusCode::NOT_FOUND {
        return Box::new(future::ok(resp));
    }

    serve(req, resp, root_dir)
}

fn serve(
    req: &Request<Body>,
    resp: Response<Body>,
    root_dir: &Path,
) -> Box<Future<Item = Response<Body>, Error = Error> + Send + 'static> {
    let path = super::local_path_for_request(req, root_dir);
    let path = path.unwrap_or(PathBuf::new());
    let ext = path.extension().and_then(OsStr::to_str).unwrap_or("");

    if ext == "md" {
        Box::new(md_path_to_html(&path))
    } else {
        Box::new(future::ok(resp))
    }
}

fn md_path_to_html(path: &Path)
                   -> impl Future<Item = Response<Body>, Error = Error>
{
    super::internal_server_error()
}
