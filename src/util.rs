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
}
