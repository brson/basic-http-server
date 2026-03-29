//! Developer extensions for basic-http-server.
//!
//! Each extension is implemented as axum middleware, keeping extension code
//! cleanly separated from the core static file server. Extensions are only
//! active when the `-x` flag is passed.

mod dir_list;
mod markdown;
mod source_text;

pub use dir_list::dir_list_middleware;
pub use markdown::markdown_middleware;
pub use source_text::source_text_middleware;
