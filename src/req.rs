use bytes::{Bytes, BytesMut};
use failure::Error;
use http::header::{HeaderName, HeaderValue};
use http::{HeaderMap, Method, Uri, Version};
use httparse::{Request, EMPTY_HEADER};
use twoway::find_bytes;

use crate::body::FramingMethod;
use crate::util::{can_keep_alive, is_chunked, maybe_content_length};

#[derive(Debug, PartialEq)]
pub struct ReqHead {
    pub method: Method,
    pub uri: Uri,
    pub version: Version,
    pub headers: HeaderMap,
}

impl ReqHead {
    fn from_buf(buf: &mut BytesMut) -> Result<Option<Self>, Error> {
        let buf = match find_bytes(buf, &b"\r\n\r\n"[..]) {
            Some(n) => buf.split_to(n + 4).freeze(),
            None => return Ok(None),
        };
        let mut hdrs = [EMPTY_HEADER; 50];
        let mut pr = Request::new(&mut hdrs);
        let s = pr.parse(&buf)?;
        debug_assert!(s.is_complete());
        let method = Method::from_bytes(pr.method.unwrap().as_bytes())?;

        let buf_start = buf.as_ref().as_ptr() as usize;

        let path = pr.path.unwrap();
        let path_start = path.as_ptr() as usize - buf_start;
        let path_end = path_start + path.len();
        let uri = Uri::from_shared(buf.slice(path_start, path_end))?;

        let version = if pr.version.unwrap() == 1 {
            Version::HTTP_11
        } else {
            Version::HTTP_10
        };

        let mut headers = HeaderMap::with_capacity(pr.headers.len());
        for hdr in pr.headers.iter() {
            let name = HeaderName::from_bytes(hdr.name.as_bytes())
                .expect("header name invalid");
            let value_start = hdr.value.as_ptr() as usize - buf_start;
            let value_end = value_start + hdr.value.len();
            let value = unsafe {
                HeaderValue::from_shared_unchecked(
                    buf.slice(value_start, value_end),
                )
            };
            headers.append(name, value);
        }

        Ok(Some(ReqHead {
            method,
            uri,
            version,
            headers,
        }))
    }

    pub fn write_to_buf(&self, buf: &mut BytesMut) -> Bytes {
        let mut n = 0;
        buf.extend_from_slice(self.method.as_str().as_bytes());
        n += self.method.as_str().len();
        buf.extend_from_slice(b" ");
        n += 1;
        buf.extend_from_slice(self.uri.path().as_bytes());
        n += self.uri.path().len();
        if let Some(qs) = self.uri.query() {
            buf.extend_from_slice(b"?");
            n += 1;
            buf.extend_from_slice(qs.as_bytes());
            n += qs.len();
        }
        buf.extend_from_slice(b" ");
        n += 1;
        if self.version == Version::HTTP_11 {
            buf.extend_from_slice(b"HTTP/1.1");
            n += 8;
        } else {
            unreachable!();
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

    pub fn can_keep_alive(&self) -> bool {
        can_keep_alive(self.version, &self.headers)
    }

    fn framing_method(&self) -> FramingMethod {
        if is_chunked(&self.headers) {
            FramingMethod::Chunked
        } else {
            FramingMethod::ContentLength(
                maybe_content_length(&self.headers).unwrap_or(0),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use http::header::{CONNECTION, CONTENT_LENGTH, HOST, TRANSFER_ENCODING};

    #[test]
    fn parse_simple_request() {
        let req_text = &b"GET /a HTTP/1.1\r\n\
                       host: example.com\r\n\
                       connection: close\r\n\r\n"[..];
        assert_eq!(
            ReqHead {
                method: Method::GET,
                uri: "/a".parse().unwrap(),
                version: Version::HTTP_11,
                headers: vec![
                    (HOST, HeaderValue::from_static("example.com")),
                    (CONNECTION, HeaderValue::from_static("close")),
                ]
                .into_iter()
                .collect(),
            },
            ReqHead::from_buf(&mut req_text.into())
                .expect("parsed request")
                .expect("complete request")
        );
    }
}
