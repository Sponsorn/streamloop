//! The build's version: the workspace version, or `STREAMLOOP_BUILD_VERSION` when it was set at build time.

pub const VERSION: &str = env!("STREAMLOOP_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert!(!VERSION.is_empty());
    }
}
