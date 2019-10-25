use std::fmt;

use bytes::BytesMut;
use http::header::{HeaderName, HeaderValue};
use http::HeaderMap;
use httparse::{parse_chunk_size, parse_headers, Status, EMPTY_HEADER};

use crate::event::Event;

pub use self::writer::BodyWriter;

pub mod writer {
    use std::io::{Cursor, Write};
    use std::mem::size_of;

    use crate::body::{BodyError, BodyResult};
    use bytes::{BufMut, Bytes, BytesMut};

    #[derive(Clone, Copy, Debug)]
    pub enum BodyWriter {
        ContentLength(ContentLength),
        Chunked,
        Http10,
    }

    #[derive(Clone, Copy, Debug)]
    pub struct ContentLength(usize);

    impl ContentLength {
        fn write_chunk(&mut self, data: Bytes) -> BodyResult<Bytes> {
            if data.len() < self.0 {
                return Err(BodyError::TooMuchData);
            }
            self.0 -= data.len();
            Ok(data)
        }
    }

    fn write_chunked_chunk(
        buf: &mut BytesMut,
        data: &Bytes,
    ) -> BodyResult<Bytes> {
        if buf.capacity() < (4 + size_of::<usize>() + data.len()) {
            buf.reserve(4 + size_of::<usize>() + data.len());
        }
        // XXX: this will need pretty extensive tests
        unsafe {
            buf.set_len(0);
            let n = {
                let mut cur = Cursor::new(buf.bytes_mut());
                write!(&mut cur, "{:x}\r\n", data.len())?;
                cur.position() as usize
            };
            buf.advance_mut(n);
        }
        buf.extend_from_slice(data);
        buf.extend_from_slice(b"\r\n");
        Ok(buf.take().freeze())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FramingMethod {
    ContentLength(usize),
    Chunked,
    Http10,
}

#[derive(Clone, Copy, Debug)]
pub enum BodyReader {
    ContentLength(ContentLength),
    Chunked(Chunked),
    Http10,
}

impl BodyReader {
    pub(crate) fn next_event(
        &mut self,
        buf: &mut BytesMut,
    ) -> BodyResult<Option<Event>> {
        match *self {
            Self::ContentLength(ref mut r) => r.next_event(buf),
            Self::Chunked(ref mut r) => r.next_event(buf),
            Self::Http10 => Http10::next_event(buf),
        }
    }

    pub(crate) fn eof(&self) -> BodyResult<Event> {
        match *self {
            Self::ContentLength(_) | Self::Chunked(_) => {
                Err(BodyError::ConnectionClosedPrematurely)
            }
            Self::Http10 => Ok(Event::EndOfMessage(None)),
        }
    }
}

impl From<FramingMethod> for BodyReader {
    fn from(m: FramingMethod) -> Self {
        match m {
            FramingMethod::ContentLength(n) => {
                Self::ContentLength(ContentLength(n))
            }
            FramingMethod::Chunked => Self::Chunked(Chunked::Start),
            FramingMethod::Http10 => Self::Http10,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ContentLength(usize);

impl ContentLength {
    fn next_event(&mut self, buf: &mut BytesMut) -> BodyResult<Option<Event>> {
        if self.0 == 0 {
            return Ok(Some(Event::EndOfMessage(None)));
        }
        let data_buf = buf.split_to(self.0.min(buf.len()));
        if data_buf.is_empty() {
            return Ok(None);
        }
        self.0 -= data_buf.len();
        Ok(Some(Event::Data(data_buf.freeze())))
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Chunked {
    Start,
    Data(usize),
    End,
    Trailers,
}

#[derive(Clone, Copy, Debug)]
struct HeaderPos {
    name: (usize, usize),
    value: (usize, usize),
}

impl HeaderPos {
    fn new() -> Self {
        Self {
            name: (0, 0),
            value: (0, 0),
        }
    }
}

impl Chunked {
    fn next_event(&mut self, buf: &mut BytesMut) -> BodyResult<Option<Event>> {
        use self::Chunked::*;

        loop {
            match *self {
                Start => {
                    let r = parse_chunk_size(buf);
                    if r.is_err() {
                        return Err(BodyError::InvalidChunkSize);
                    }
                    let st = r.unwrap();
                    match st {
                        Status::Complete((consume, chunk_size)) => {
                            buf.split_to(consume);
                            *self = if chunk_size == 0 {
                                Trailers
                            } else {
                                Data(chunk_size as usize)
                            };
                            continue;
                        }
                        Status::Partial => return Ok(None),
                    }
                }
                Data(ref mut rem) => {
                    let data_buf = buf.split_to((*rem).min(buf.len()));
                    if data_buf.is_empty() {
                        return Ok(None);
                    }
                    if *rem == data_buf.len() {
                        *self = End;
                    } else {
                        *rem -= data_buf.len();
                    }
                    return Ok(Some(Event::Data(data_buf.freeze())));
                }
                End => {
                    if buf.len() < 2 {
                        return Ok(None);
                    }
                    buf.split_to(2);
                    *self = Start;
                    continue;
                }
                Trailers => {
                    // XXX: this is in serious need of cleanup. It would be
                    //      incredibly nice if httparse returned offsets
                    //      instead of slices
                    let mut hdr_pos = [HeaderPos::new(); 20];
                    let (consume, hdr_pos) = {
                        let mut hdrs = [EMPTY_HEADER; 20];
                        match parse_headers(&buf, &mut hdrs)? {
                            Status::Complete((n, hdrs)) => {
                                debug_assert!(hdrs.len() <= hdr_pos.len());
                                let buf_start = buf.as_ref().as_ptr() as usize;
                                let hdr_pos = &mut hdr_pos[..hdrs.len()];
                                for (hdr, ref mut hdr_pos) in
                                    hdrs.iter().zip(hdr_pos.iter_mut())
                                {
                                    let name_start =
                                        hdr.name.as_bytes().as_ptr() as usize
                                            - buf_start;
                                    let name_end = name_start + hdr.name.len();
                                    let value_start = hdr.value.as_ptr()
                                        as usize
                                        - buf_start;
                                    let value_end =
                                        value_start + hdr.value.len();
                                    hdr_pos.name = (name_start, name_end);
                                    hdr_pos.value = (value_start, value_end);
                                }
                                (n, hdr_pos)
                            }
                            Status::Partial => return Ok(None),
                        }
                    };
                    let hdr_buf = buf.split_to(consume).freeze();

                    if hdr_pos.is_empty() {
                        return Ok(Some(Event::EndOfMessage(None)));
                    }

                    let mut headers = HeaderMap::with_capacity(hdr_pos.len());
                    for hdr_pos in hdr_pos.iter() {
                        let (name_start, name_end) = hdr_pos.name;
                        let name = HeaderName::from_bytes(
                            &hdr_buf.slice(name_start, name_end),
                        )
                        .expect("header name already valid");
                        let (value_start, value_end) = hdr_pos.value;
                        let value = unsafe {
                            HeaderValue::from_shared_unchecked(
                                hdr_buf.slice(value_start, value_end),
                            )
                        };
                        headers.append(name, value);
                    }
                    return Ok(Some(Event::EndOfMessage(Some(headers))));
                }
            }
        }
    }
}

struct Http10;

impl Http10 {
    fn next_event(buf: &mut BytesMut) -> BodyResult<Option<Event>> {
        Ok(if buf.is_empty() {
            None
        } else {
            Some(Event::Data(buf.split_to(buf.len()).freeze()))
        })
    }
}

#[derive(Debug)]
pub enum BodyError {
    TooMuchData,
    ConnectionClosedPrematurely,
    InvalidChunkSize,
    IO(std::io::Error),
    HttpParse(httparse::Error),
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::TooMuchData => write!(f, "Too much data to write"),
            Self::ConnectionClosedPrematurely => {
                write!(f, "connection closed before finishing body")
            }
            Self::InvalidChunkSize => write!(f, "invalid chunk size"),
            Self::IO(e) => write!(f, "An IO error occurred: {}", e),
            Self::HttpParse(e) => {
                write!(f, "An error occurred when parsing HTTP: {}", e)
            }
        }
    }
}

impl std::error::Error for BodyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IO(e) => Some(e),
            Self::HttpParse(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for BodyError {
    fn from(e: std::io::Error) -> Self {
        Self::IO(e)
    }
}

impl From<httparse::Error> for BodyError {
    fn from(e: httparse::Error) -> Self {
        Self::HttpParse(e)
    }
}

pub type BodyResult<T> = std::result::Result<T, BodyError>;

#[cfg(test)]
mod tests {
    use super::*;

    mod content_length {
        use super::*;

        #[test]
        fn empty() {
            let mut r = ContentLength(0);
            let buf = &b""[..];
            assert_eq!(
                Event::EndOfMessage(None),
                r.next_event(&mut buf.into()).unwrap().unwrap(),
            );
        }

        #[test]
        fn len_10() {
            let mut r = ContentLength(10);
            let buf = &b"0123456789"[..];
            assert_eq!(
                Event::Data(buf.into()),
                r.next_event(&mut buf.into()).unwrap().unwrap(),
            );
        }
    }

    mod chunked {
        use super::*;

        #[test]
        fn empty_no_trailers() {
            let mut r = Chunked::Start;
            let buf = &b"0\r\n\r\n"[..];
            assert_eq!(
                Event::EndOfMessage(None),
                r.next_event(&mut buf.into()).unwrap().unwrap(),
            );
        }

        #[test]
        fn empty_single_trailer() {
            let mut r = Chunked::Start;
            let buf = &b"0\r\nSome: header\r\n\r\n"[..];
            assert_eq!(
                Event::EndOfMessage(Some(
                    vec![(
                        HeaderName::from_lowercase(b"some")
                            .expect("valid header name"),
                        HeaderValue::from_static("header"),
                    )]
                    .into_iter()
                    .collect()
                )),
                r.next_event(&mut buf.into()).unwrap().unwrap(),
            );
        }

        #[test]
        fn two_chunks() {
            let mut r = Chunked::Start;
            let mut buf = b"5\r\n\
                          01234\r\n\
                          10\r\n\
                          0123456789abcdef\r\n\
                          0\r\n\
                          \r\n"[..]
                .into();
            assert_eq!(
                Event::Data(b"01234"[..].into()),
                r.next_event(&mut buf).expect("read 5 bytes").unwrap(),
            );
            assert_eq!(
                Event::Data(b"0123456789abcdef"[..].into()),
                r.next_event(&mut buf).expect("read 5 bytes").unwrap(),
            );
            assert_eq!(
                Event::EndOfMessage(None),
                r.next_event(&mut buf).unwrap().unwrap(),
            );
        }
    }
}
