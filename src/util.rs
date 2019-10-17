use std::str;

use http::{HeaderMap, Version};

pub fn can_keep_alive(version: Version, headers: &HeaderMap) -> bool {
    use http::header::CONNECTION;

    !(version < Version::HTTP_11
        || headers.get_all(CONNECTION).into_iter().any(|val| {
            str::from_utf8(val.as_bytes())
                .map(|s| {
                    s.split(',')
                        .any(|tok| tok.trim().eq_ignore_ascii_case("close"))
                })
                .unwrap_or(false)
        }))
}

pub fn is_chunked(headers: &HeaderMap) -> bool {
    use http::header::TRANSFER_ENCODING;

    headers
        .get_all(TRANSFER_ENCODING)
        .iter()
        .next_back()
        .and_then(|v| str::from_utf8(v.as_bytes()).ok())
        .and_then(|s| {
            s.rsplit(',')
                .next()
                .map(|tok| tok.trim().eq_ignore_ascii_case("chunked"))
        })
        .unwrap_or(false)
}

pub fn maybe_content_length(headers: &HeaderMap) -> Option<usize> {
    use http::header::CONTENT_LENGTH;

    headers
        .get(CONTENT_LENGTH)
        .and_then(|tok| tok.to_str().ok().and_then(|s| s.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use http::header::{
        HeaderValue, CONNECTION, CONTENT_LENGTH, HOST, TRANSFER_ENCODING,
    };

    #[test]
    fn keep_alive() {
        assert!(can_keep_alive(
            Version::HTTP_11,
            &vec![(HOST, HeaderValue::from_static("example.com"))]
                .into_iter()
                .collect()
        ));
    }

    #[test]
    fn connection_close_disables_keep_alive() {
        assert!(!can_keep_alive(
            Version::HTTP_11,
            &vec![
                (HOST, HeaderValue::from_static("example.com")),
                (CONNECTION, HeaderValue::from_static("close"))
            ]
            .into_iter()
            .collect()
        ));
    }

    #[test]
    fn http_10_disables_keep_alive() {
        assert!(!can_keep_alive(Version::HTTP_10, &HeaderMap::new()));
    }

    #[test]
    fn is_chunked_with_header() {
        assert!(is_chunked(
            &vec![(TRANSFER_ENCODING, HeaderValue::from_static("chunked"))]
                .into_iter()
                .collect()
        ));
    }

    #[test]
    fn is_not_chunked_without_header() {
        assert!(!is_chunked(&HeaderMap::new()));
    }

    #[test]
    fn maybe_content_length_none_on_no_header() {
        assert!(maybe_content_length(&HeaderMap::new()).is_none());
    }

    #[test]
    fn maybe_content_length_parses_decimal() {
        assert_eq!(
            Some(100),
            maybe_content_length(
                &vec![(CONTENT_LENGTH, HeaderValue::from_static("100"))]
                    .into_iter()
                    .collect()
            )
        );
    }
}
