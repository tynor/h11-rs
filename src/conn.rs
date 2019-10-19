use std::io::Read;
use std::marker::PhantomData;
use std::str;

use bytes::{BufMut, Bytes, BytesMut};
use failure::{format_err, Error};
use http::{HeaderMap, Method, StatusCode, Version};

use crate::body::BodyReader;
use crate::event::Event;
use crate::req::ReqHead;
use crate::resp::RespHead;
use crate::state::{self, State, SwitchEvent};

#[allow(clippy::empty_enum)]
pub enum Client {}

#[allow(clippy::empty_enum)]
pub enum Server {}

pub struct HttpConn<Role> {
    inner: Inner,
    pd: PhantomData<Role>,
}

impl<Role> HttpConn<Role> {
    pub fn new() -> Self {
        Self::from_bufs(8192, BytesMut::new(), BytesMut::new())
    }

    pub fn from_bufs(
        max_event_size: usize,
        in_buf: BytesMut,
        out_buf: BytesMut,
    ) -> Self {
        Self {
            inner: Inner::from_bufs(max_event_size, in_buf, out_buf),
            pd: PhantomData,
        }
    }

    pub fn into_bufs(self) -> (BytesMut, BytesMut) {
        self.inner.into_bufs()
    }

    pub fn read_from<R: Read>(&mut self, r: &mut R) -> Result<usize, Error> {
        self.inner.read_from(r)
    }
}

impl<Role> Default for HttpConn<Role> {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpConn<Client> {
    pub fn send_req(&mut self, req: ReqHead) -> Result<Bytes, Error> {
        let event = Event::Request(req);
        self.inner.client_event(&event)?;
        Ok(self.inner.write_event(event))
    }

    pub fn send_data(&mut self, data: Bytes) -> Result<Bytes, Error> {
        let event = Event::Data(data);
        self.inner.client_event(&event)?;
        Ok(self.inner.write_event(event))
    }

    pub fn send_end_of_message(
        &mut self,
        headers: Option<HeaderMap>,
    ) -> Result<Bytes, Error> {
        let event = Event::EndOfMessage(headers);
        self.inner.client_event(&event)?;
        Ok(self.inner.write_event(event))
    }

    pub fn send_connection_closed(&mut self) -> Result<Bytes, Error> {
        self.inner.client_event(&Event::ConnectionClosed)?;
        Ok(Bytes::new())
    }
}

impl HttpConn<Server> {
    pub fn next_event(&mut self) -> Result<Option<Event>, Error> {
        self.inner.next_client_event()
    }

    pub fn send_info_resp(&mut self, resp: RespHead) -> Result<Bytes, Error> {
        let event = Event::InfoResponse(resp);
        self.inner.server_event(&event)?;
        Ok(self.inner.write_event(event))
    }

    pub fn send_resp(&mut self, resp: RespHead) -> Result<Bytes, Error> {
        let event = Event::Response(resp);
        self.inner.server_event(&event)?;
        Ok(self.inner.write_event(event))
    }

    pub fn send_data(&mut self, data: Bytes) -> Result<Bytes, Error> {
        let event = Event::Data(data);
        self.inner.server_event(&event)?;
        Ok(self.inner.write_event(event))
    }

    pub fn send_end_of_message(
        &mut self,
        headers: Option<HeaderMap>,
    ) -> Result<Bytes, Error> {
        let event = Event::EndOfMessage(headers);
        self.inner.server_event(&event)?;
        Ok(self.inner.write_event(event))
    }

    pub fn send_connection_closed(&mut self) -> Result<Bytes, Error> {
        self.inner.server_event(&Event::ConnectionClosed)?;
        Ok(Bytes::new())
    }
}

struct Inner {
    state: State,
    max_event_size: usize,
    in_buf: BytesMut,
    in_buf_closed: bool,
    out_buf: BytesMut,
    client_wants_continue: bool,
    body_reader: Option<BodyReader>,
    peer_http_version: Option<Version>,
}

impl Inner {
    fn from_bufs(
        max_event_size: usize,
        in_buf: BytesMut,
        out_buf: BytesMut,
    ) -> Self {
        Self {
            state: State::new(),
            max_event_size,
            in_buf,
            in_buf_closed: false,
            out_buf,
            client_wants_continue: false,
            body_reader: None,
            peer_http_version: None,
        }
    }

    fn into_bufs(self) -> (BytesMut, BytesMut) {
        (self.in_buf, self.out_buf)
    }

    // XXX: this should be able to indicate that it will *never* return
    //      an event again, because the connection has been hijacked via
    //      UPGRADE or CONNECT
    fn next_client_event(&mut self) -> Result<Option<Event>, Error> {
        use state::Client::*;

        match self.state.states().0 {
            Idle => match ReqHead::from_buf(&mut self.in_buf) {
                Ok(Some(r)) => {
                    let br = BodyReader::from(r.framing_method());
                    let event = Event::Request(r);
                    self.client_event(&event)?;
                    self.body_reader = Some(br);
                    Ok(Some(event))
                }
                Ok(None) => Ok(None),
                Err(e) => {
                    self.state = self.state.client_error();
                    Err(e)
                }
            },
            SendBody => {
                let br = self.body_reader.as_mut().expect("reading body");
                if !self.in_buf.is_empty() {
                    br.next_event(&mut self.in_buf)
                } else if self.in_buf_closed {
                    br.eof().map(Some)
                } else {
                    Ok(None)
                }
            }
            Error => Err(format_err!("client in error state")),
            Done | MustClose | Closed | MightSwitchProtocol
            | SwitchedProtocol => Ok(None),
        }
    }

    fn read_from<R: Read>(&mut self, r: &mut R) -> Result<usize, Error> {
        if self.in_buf.remaining_mut() < self.max_event_size {
            self.in_buf.reserve(self.max_event_size);
        }
        unsafe {
            r.read(self.in_buf.bytes_mut())
                .map_err(|e| e.into())
                .and_then(|n| {
                    if n == 0 {
                        self.in_buf_closed = true;
                    } else {
                        if self.in_buf_closed {
                            return Err(format_err!(
                                "peer closed then sent data??"
                            ));
                        }
                        self.in_buf.advance_mut(n);
                    }
                    Ok(n)
                })
        }
    }

    fn write_event(&mut self, event: Event) -> Bytes {
        event.into_buf(&mut self.out_buf)
    }

    fn client_event(&mut self, event: &Event) -> Result<(), Error> {
        use http::header::{EXPECT, UPGRADE};

        if let Event::Request(ref req) = *event {
            if req.method == Method::CONNECT {
                self.state = self.state.connect_proposal();
            }
            if req.headers.contains_key(UPGRADE) {
                self.state = self.state.upgrade_proposal();
            }
        }

        self.state = self.state.client_event(event.to_state_event())?;

        match *event {
            Event::Request(ref req) => {
                if !req.can_keep_alive() {
                    self.state = self.state.disable_keep_alive();
                }
                self.client_wants_continue = req
                    .headers
                    .get_all(EXPECT)
                    .iter()
                    .next_back()
                    .map_or(false, |tok| {
                        str::from_utf8(tok.as_bytes())
                            .map(|s| {
                                s.trim().eq_ignore_ascii_case("100-continue")
                            })
                            .unwrap_or(false)
                    });
            }
            Event::Data(_) | Event::EndOfMessage(_) => {
                self.client_wants_continue = false;
            }
            _ => {}
        }
        Ok(())
    }

    fn server_event(&mut self, event: &Event) -> Result<(), Error> {
        let switch = match *event {
            Event::InfoResponse(RespHead {
                status: StatusCode::SWITCHING_PROTOCOLS,
                ..
            }) => Some(SwitchEvent::Upgrade),
            Event::Response(RespHead { status, .. })
                if self.state.pending_connect && status.is_success() =>
            {
                Some(SwitchEvent::Connect)
            }
            _ => None,
        };

        self.state =
            self.state.server_event(event.to_state_event(), switch)?;

        match *event {
            Event::InfoResponse(_) => self.client_wants_continue = false,
            Event::Response(ref resp) => {
                if !resp.can_keep_alive() {
                    self.state = self.state.disable_keep_alive();
                }
                self.client_wants_continue = false;
            }
            _ => {}
        }

        Ok(())
    }
}
