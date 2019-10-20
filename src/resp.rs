use bytes::{Bytes, BytesMut};
use err_derive::Error;
use http::header::{HeaderName, HeaderValue};
use http::{HeaderMap, Method, StatusCode, Version};
use httparse::{Response, EMPTY_HEADER};
use twoway::find_bytes;

use crate::body::FramingMethod;
use crate::util::{can_keep_alive, is_chunked, maybe_content_length};

#[derive(Debug, Error)]
pub enum RespHeadError {
    #[error(display = "An error occurred parsing HTTP: {}", _0)]
    HttpParse(#[error(source)] httparse::Error),
    #[error(display = "An invalid status code was provided: {}", _0)]
    InvalidStatusCode(#[error(source)] http::status::InvalidStatusCode),
}

#[derive(Debug, PartialEq)]
pub struct RespHead {
    pub status: StatusCode,
    pub version: Version,
    pub headers: HeaderMap,
}

impl RespHead {
    fn from_buf(buf: &mut BytesMut) -> Result<Option<Self>, RespHeadError> {
        let buf = match find_bytes(buf, &b"\r\n\r\n"[..]) {
            Some(n) => buf.split_to(n + 4).freeze(),
            None => return Ok(None),
        };
        let mut hdrs = [EMPTY_HEADER; 50];
        let mut pr = Response::new(&mut hdrs);
        let s = pr.parse(&buf)?;
        debug_assert!(s.is_complete());

        let status = StatusCode::from_u16(pr.code.unwrap())?;

        let version = if pr.version.unwrap() == 1 {
            Version::HTTP_11
        } else {
            Version::HTTP_10
        };

        let buf_start = buf.as_ref().as_ptr() as usize;

        let mut headers = HeaderMap::with_capacity(pr.headers.len());
        for hdr in pr.headers.iter() {
            let name = HeaderName::from_bytes(hdr.name.as_bytes())
                .expect("header name already valid");
            let value_start = hdr.value.as_ptr() as usize - buf_start;
            let value_end = value_start + hdr.value.len();
            let value = unsafe {
                HeaderValue::from_shared_unchecked(
                    buf.slice(value_start, value_end),
                )
            };
            headers.append(name, value);
        }

        Ok(Some(Self {
            status,
            version,
            headers,
        }))
    }

    pub(crate) fn write_to_buf(&self, buf: &mut BytesMut) -> Bytes {
        let mut n = 0;
        if self.version == Version::HTTP_11 {
            buf.extend_from_slice(b"HTTP/1.1");
            n += 8;
        } else {
            unreachable!();
        }
        buf.extend_from_slice(b" ");
        n += 1;
        buf.extend_from_slice(self.status.as_str().as_bytes());
        n += self.status.as_str().len();
        if let Some(reason) = self.status.canonical_reason() {
            buf.extend_from_slice(b" ");
            n += 1;
            buf.extend_from_slice(reason.as_bytes());
            n += reason.len();
        }
        buf.extend_from_slice(b"\r\n");
        n += 2;
        for (name, value) in self.headers.iter() {
            buf.extend_from_slice(name.as_str().as_bytes());
            n += name.as_str().len();
            buf.extend_from_slice(b": ");
            n += 2;
            buf.extend_from_slice(value.as_bytes());
            n += value.len();
            buf.extend_from_slice(b"\r\n");
            n += 2;
        }
        buf.extend_from_slice(b"\r\n");
        n += 2;
        buf.split_to(n).freeze()
    }

    pub(crate) fn can_keep_alive(&self) -> bool {
        can_keep_alive(self.version, &self.headers)
    }

    fn framing_method(&self, method: &Method) -> FramingMethod {
        if self.status == StatusCode::NO_CONTENT
            || self.status == StatusCode::NOT_MODIFIED
            || method == Method::HEAD
            || (method == Method::CONNECT && self.status.is_success())
        {
            FramingMethod::ContentLength(0)
        } else if is_chunked(&self.headers) {
            FramingMethod::Chunked
        } else {
            maybe_content_length(&self.headers)
                .map_or(FramingMethod::Http10, FramingMethod::ContentLength)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use http::header::CONNECTION;

    #[test]
    fn parse_simple_response() {
        let resp_text = &b"HTTP/1.1 200 OK \r\n\
                        connection: close\r\n\r\n"[..];
        assert_eq!(
            RespHead {
                status: StatusCode::OK,
                version: Version::HTTP_11,
                headers: vec![(CONNECTION, HeaderValue::from_static("close"))]
                    .into_iter()
                    .collect(),
            },
            RespHead::from_buf(&mut resp_text.into())
                .expect("parsed request")
                .expect("complete request")
        );
    }

    #[test]
    fn parse_response_no_headers() {
        let resp_text = &b"HTTP/1.1 200 OK\r\n\r\n"[..];
        assert_eq!(
            RespHead {
                status: StatusCode::OK,
                version: Version::HTTP_11,
                headers: HeaderMap::new(),
            },
            RespHead::from_buf(&mut resp_text.into())
                .expect("parsed request")
                .expect("complete request")
        );
    }

    #[test]
    fn parse_http_10_response() {
        let resp_text = &b"HTTP/1.0 200 OK\r\n\
                        Some: header\r\n\r\n"[..];
        assert_eq!(
            RespHead {
                status: StatusCode::OK,
                version: Version::HTTP_10,
                headers: vec![(
                    HeaderName::from_lowercase(b"some")
                        .expect("valid header name"),
                    HeaderValue::from_static("header"),
                )]
                .into_iter()
                .collect(),
            },
            RespHead::from_buf(&mut resp_text.into())
                .expect("parsed request")
                .expect("complete request")
        );
    }

    #[test]
    fn parse_empty_header_response() {
        let resp_text = &b"HTTP/1.0 200 OK\r\n\
                        Foo:\r\n\r\n"[..];
        assert_eq!(
            RespHead {
                status: StatusCode::OK,
                version: Version::HTTP_10,
                headers: vec![(
                    HeaderName::from_lowercase(b"foo")
                        .expect("valid header name"),
                    HeaderValue::from_static(""),
                )]
                .into_iter()
                .collect(),
            },
            RespHead::from_buf(&mut resp_text.into())
                .expect("parsed request")
                .expect("complete request")
        );
    }

    #[test]
    fn parse_ws_only_header_response() {
        let resp_text = &b"HTTP/1.0 200 OK\r\n\
                        Foo: \t \t \r\n\r\n"[..];
        assert_eq!(
            RespHead {
                status: StatusCode::OK,
                version: Version::HTTP_10,
                headers: vec![(
                    HeaderName::from_lowercase(b"foo")
                        .expect("valid header name"),
                    HeaderValue::from_static(""),
                )]
                .into_iter()
                .collect(),
            },
            RespHead::from_buf(&mut resp_text.into())
                .expect("parsed request")
                .expect("complete request")
        );
    }
}
