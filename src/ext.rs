use crate::Error;
use futures::{future, Future};
use http::{Request, Response, StatusCode};
use hyper::Body;
use std::path::PathBuf;

pub fn map(req: &Request<Body>,
           resp: Response<Body>,
           root_dir: &PathBuf)
           -> Box<Future<Item = Response<Body>, Error = Error> + Send + 'static>
{
    if resp.status() != StatusCode::NOT_FOUND {
        return Box::new(future::ok(resp));
    }

    Box::new(serve(req, root_dir))
}

fn serve(
    req: &Request<Body>,
    root_dir: &PathBuf,
) -> impl Future<Item = Response<Body>, Error = Error> {
    super::internal_server_error()
}
