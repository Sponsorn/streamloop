//! The check both local APIs (the control process on 7654, the supervisor on 4417) make before
//! anything else: a request from a browser is refused, so a web page open on the streaming box
//! cannot stop or steer the stream.

/// The error text of the 403 a refused request gets.
pub const REFUSED: &str = "Forbidden: requests from a browser are refused";

/// True when the request must be refused: a Host other than 127.0.0.1:port or localhost:port
/// (case-insensitive; a missing Host counts as other), or any Origin header at all.
///
/// The Host check stops DNS rebinding, where a page reaches the port under its own name; the Origin
/// check stops a cross-site POST, which a browser sends with an Origin even without CORS.
pub fn refused(host: Option<&str>, origin: Option<&str>, port: u16) -> bool {
    if origin.is_some() {
        return true;
    }
    let Some(host) = host else { return true };
    let host = host.to_ascii_lowercase();
    host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_loopback_hosts_on_the_port() {
        assert!(!refused(Some("127.0.0.1:7654"), None, 7654));
        assert!(!refused(Some("localhost:7654"), None, 7654));
        assert!(!refused(Some("LocalHost:7654"), None, 7654));
    }

    #[test]
    fn refuses_another_host_or_port() {
        assert!(refused(Some("evil.example:7654"), None, 7654));
        assert!(refused(Some("127.0.0.1:4417"), None, 7654));
        assert!(refused(Some("127.0.0.1"), None, 7654));
        assert!(refused(Some("localhost"), None, 7654));
        assert!(refused(Some("[::1]:7654"), None, 7654));
        assert!(refused(Some("127.0.0.1:7654.evil.example"), None, 7654));
    }

    #[test]
    fn refuses_a_missing_host() {
        assert!(refused(None, None, 7654));
    }

    #[test]
    fn refuses_any_origin() {
        assert!(refused(Some("127.0.0.1:7654"), Some("http://evil.example"), 7654));
        assert!(refused(Some("127.0.0.1:7654"), Some("null"), 7654));
        assert!(refused(Some("127.0.0.1:7654"), Some(""), 7654));
    }
}
