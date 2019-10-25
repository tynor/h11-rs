#![allow(dead_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::module_name_repetitions)]
// This lint causes false positives because of how many "\r\n" line
// endings there are in HTTP.
#![allow(clippy::write_with_newline)]

mod body;
mod conn;
mod event;
mod req;
mod resp;
mod state;
mod util;

pub use conn::{Client, HttpConn, Server};
pub use event::Event;
pub use req::ReqHead;
pub use resp::RespHead;

pub mod error {
    pub use crate::conn::Error;

    pub type Result<T> = std::result::Result<T, Error>;
}
