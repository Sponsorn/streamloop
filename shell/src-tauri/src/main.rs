#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::{Url, WebviewUrl, WebviewWindowBuilder};

const BACKEND_PORT: u16 = 7654;

/// The window may show the bundled fallback page and the local backend, nothing else.
fn is_allowed(url: &Url) -> bool {
    match (url.scheme(), url.host_str()) {
        ("about", _) => url.as_str() == "about:blank",
        ("http" | "https", Some("tauri.localhost")) => true,
        ("http", Some("127.0.0.1")) => url.port() == Some(BACKEND_PORT),
        _ => false,
    }
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("StreamLoop")
                .inner_size(1280.0, 800.0)
                .on_navigation(is_allowed)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the StreamLoop shell");
}

#[cfg(test)]
mod tests {
    use super::is_allowed;
    use tauri::Url;

    fn allowed(s: &str) -> bool {
        is_allowed(&Url::parse(s).unwrap())
    }

    #[test]
    fn allows_the_bundled_fallback_page() {
        assert!(allowed("http://tauri.localhost/index.html"));
        assert!(allowed("https://tauri.localhost/"));
    }

    #[test]
    fn allows_the_backend_dashboard() {
        assert!(allowed("http://127.0.0.1:7654/admin"));
        assert!(allowed("http://127.0.0.1:7654/"));
    }

    #[test]
    fn allows_the_blank_page_webview2_starts_on() {
        assert!(allowed("about:blank"));
    }

    #[test]
    fn blocks_external_sites() {
        assert!(!allowed("https://www.youtube.com/watch?v=abc"));
        assert!(!allowed("https://dev.twitch.tv/console/apps"));
    }

    #[test]
    fn blocks_other_ports_and_schemes_on_loopback() {
        assert!(!allowed("http://127.0.0.1:8080/"));
        assert!(!allowed("http://127.0.0.1/"));
        assert!(!allowed("https://127.0.0.1:7654/admin"));
    }

    #[test]
    fn blocks_hosts_that_only_look_like_loopback() {
        assert!(!allowed("http://127.0.0.1.example.com:7654/admin"));
        assert!(!allowed("http://tauri.localhost.example.com/"));
    }

    #[test]
    fn blocks_non_web_schemes() {
        assert!(!allowed("file:///C:/Windows/win.ini"));
    }
}
