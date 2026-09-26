use std::error::Error;
use std::fmt::Write;

/// An error and every cause behind it, as one line for a log or an IPC reply.
pub fn error_chain(error: &dyn Error) -> String {
    let mut line = error.to_string();
    let mut cause = error.source();
    while let Some(source) = cause {
        let _ = write!(line, ": {source}");
        cause = source.source();
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("outer")]
    struct Outer(#[source] std::io::Error);

    #[test]
    fn causes_follow_the_error() {
        let error = Outer(std::io::Error::other("inner"));
        assert_eq!(error_chain(&error), "outer: inner");
    }
}
