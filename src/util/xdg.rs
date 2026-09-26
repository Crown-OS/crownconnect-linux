use std::ffi::OsString;
use std::path::PathBuf;

/// Resolves an XDG base directory: the variable when it holds an absolute path, otherwise
/// `$HOME/<home_relative>`.
pub fn base_directory(variable: &str, home_relative: &str) -> Option<PathBuf> {
    absolute(std::env::var_os(variable))
        .or_else(|| absolute(std::env::var_os("HOME")).map(|home| home.join(home_relative)))
}

/// `$XDG_RUNTIME_DIR`, which has no home fallback because it must be private to the session.
pub fn runtime_directory() -> Option<PathBuf> {
    absolute(std::env::var_os("XDG_RUNTIME_DIR"))
}

fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_values_are_ignored() {
        assert_eq!(absolute(Some("relative/dir".into())), None);
        assert_eq!(absolute(None), None);
        assert_eq!(
            absolute(Some("/run/user/1000".into())),
            Some(PathBuf::from("/run/user/1000"))
        );
    }
}
