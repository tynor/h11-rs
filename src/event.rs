use bytes::{Bytes, BytesMut};
use http::HeaderMap;

use crate::req::ReqHead;
use crate::resp::RespHead;
use crate::state::StateEvent;

#[allow(clippy::large_enum_variant)]
#[derive(Debug, PartialEq)]
pub enum Event {
    Request(ReqHead),
    InfoResponse(RespHead),
    Response(RespHead),
    Data(Bytes),
    EndOfMessage(Option<HeaderMap>),
    ConnectionClosed,
}

impl Event {
    pub fn to_state_event(&self) -> StateEvent {
        use self::StateEvent::*;

        match *self {
            Event::Request(_) => Request,
            Event::InfoResponse(_) => InfoResponse,
            Event::Response(_) => Response,
            Event::Data(_) => Data,
            Event::EndOfMessage(_) => EndOfMessage,
            Event::ConnectionClosed => ConnectionClosed,
        }
    }

    pub fn into_buf(self, buf: &mut BytesMut) -> Bytes {
        use self::Event::*;

        match self {
            Request(req) => req.write_to_buf(buf),
            InfoResponse(resp) | Response(resp) => resp.write_to_buf(buf),
            Data(b) => b,
            EndOfMessage(Some(hdrs)) => {
                let mut n = 0;
                for (name, value) in hdrs.iter() {
                    buf.extend_from_slice(name.as_str().as_bytes());
                    n += name.as_str().len();
                    buf.extend_from_slice(b": ");
                    n += 2;
                    buf.extend_from_slice(value.as_bytes());
                    n += value.len();
                    buf.extend_from_slice(b"\r\n");
                    n += 2;
                }
                buf.split_to(n).freeze()
            }
            EndOfMessage(None) | ConnectionClosed => Bytes::new(),
        }
    }
}
