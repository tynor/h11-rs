#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FramingMethod {
    ContentLength(usize),
    Chunked,
    Http10,
}
