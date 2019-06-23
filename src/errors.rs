use std::io;

/// The basic-http-server error type
#[derive(From, Debug, Error, Display)]
pub enum Error {
    #[display(fmt = "failed to render template")]
    Handlebars(#[error(cause)] handlebars::TemplateRenderError),

    #[display(fmt = "i/o error")]
    Io(#[error(cause)] io::Error),

    #[display(fmt = "http error")]
    HttpError(#[error(cause)] http::Error),

    #[display(fmt = "failed to parse IP address")]
    AddrParse(#[error(cause)] std::net::AddrParseError),

    #[display(fmt = "failed to parse a number")]
    ParseInt(#[error(cause)] std::num::ParseIntError),

    #[display(fmt = "failed to parse a boolean")]
    ParseBool(#[error(cause)] std::str::ParseBoolError),

    #[display(fmt = "string is not UTF-8")]
    ParseUtf8(#[error(cause)] std::string::FromUtf8Error),

    #[display(fmt = "markdown is not UTF-8")]
    MarkdownUtf8,

    #[display(fmt = "failed to convert URL to local file path")]
    UrlToPath,

    #[display(fmt = "formatting error")]
    Fmt(#[error(cause)] std::fmt::Error),

    #[display(fmt = "failed to strip prefix")]
    StripPrefix(#[error(cause)] std::path::StripPrefixError),
}

pub type Result<T> = std::result::Result<T, Error>;
