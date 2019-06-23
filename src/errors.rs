use std::io;
use std::error::Error as StdError;

/// The basic-http-server error type
#[derive(From, Debug, Display)]
pub enum Error {
    #[display(fmt = "failed to render template")]
    Handlebars(handlebars::TemplateRenderError),

    #[display(fmt = "i/o error")]
    Io(io::Error),

    #[display(fmt = "http error")]
    HttpError(http::Error),

    #[display(fmt = "failed to parse IP address")]
    AddrParse(std::net::AddrParseError),

    #[display(fmt = "failed to parse a number")]
    ParseInt(std::num::ParseIntError),

    #[display(fmt = "failed to parse a boolean")]
    ParseBool(std::str::ParseBoolError),

    #[display(fmt = "string is not UTF-8")]
    ParseUtf8(std::string::FromUtf8Error),

    #[display(fmt = "markdown is not UTF-8")]
    MarkdownUtf8,

    #[display(fmt = "failed to convert URL to local file path")]
    UrlToPath,

    #[display(fmt = "formatting error")]
    Fmt(std::fmt::Error),

    #[display(fmt = "failed to strip prefix")]
    StripPrefix(std::path::StripPrefixError),
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        None
    }
}

pub type Result<T> = std::result::Result<T, Error>;
