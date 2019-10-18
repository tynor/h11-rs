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
