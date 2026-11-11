use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use streamloop_pot::http::{serve, serve_with, Service, MINT_QUEUE, MINT_WORKERS};
use streamloop_pot::session::{PotAnswer, PotRequest};

struct Slow {
    delay: Duration,
    started: AtomicUsize,
}

impl Slow {
    fn new(delay: Duration) -> Arc<Self> {
        Arc::new(Self { delay, started: AtomicUsize::new(0) })
    }
}

impl Service for Slow {
    fn get_pot(&self, r: &PotRequest) -> Result<PotAnswer, String> {
        self.started.fetch_add(1, Ordering::SeqCst);
        thread::sleep(self.delay);
        Ok(PotAnswer {
            content_binding: r.content_binding.clone(),
            po_token: "tok".into(),
            expires_at: UNIX_EPOCH + Duration::from_secs(1_794_139_200),
        })
    }
    fn invalidate_caches(&self) {}
    fn invalidate_it(&self) {}
    fn minter_keys(&self) -> Vec<String> {
        vec![]
    }
}

struct Panics;

impl Service for Panics {
    fn get_pot(&self, _: &PotRequest) -> Result<PotAnswer, String> {
        panic!("a bug in the mint path")
    }
    fn invalidate_caches(&self) {}
    fn invalidate_it(&self) {}
    fn minter_keys(&self) -> Vec<String> {
        vec![]
    }
}

fn start(svc: Arc<Slow>) -> (streamloop_pot::http::Running, SocketAddr) {
    let running = serve("127.0.0.1:0".parse().unwrap(), svc).expect("bind a free loopback port");
    let addr = running.addr();
    (running, addr)
}

/// One request on its own connection; the whole reply as text.
fn request(addr: SocketAddr, raw: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    s.write_all(raw.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

fn get_pot_request() -> String {
    let body = r#"{"content_binding":"vd"}"#;
    format!(
        "POST /get_pot HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[test]
fn never_sends_cors_headers() {
    let (_running, addr) = start(Slow::new(Duration::ZERO));
    let options = request(
        addr,
        "OPTIONS /get_pot HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://evil.example\r\nAccess-Control-Request-Method: POST\r\nConnection: close\r\n\r\n",
    );
    assert!(options.starts_with("HTTP/1.1 403"), "{options}");
    let post = request(addr, &get_pot_request().replace("Host:", "Origin: http://evil.example\r\nHost:"));
    assert!(post.starts_with("HTTP/1.1 403"), "{post}");
    assert!(post.contains("Browser-originated requests are not allowed"), "{post}");
    let plain = request(addr, &get_pot_request());
    assert!(plain.starts_with("HTTP/1.1 200"), "{plain}");
    for reply in [options, post, plain] {
        assert!(!reply.to_ascii_lowercase().contains("access-control-"), "{reply}");
    }
}

#[test]
fn ping_answers_while_a_mint_is_slow() {
    let svc = Slow::new(Duration::from_secs(5));
    let (_running, addr) = start(svc.clone());
    let slow = thread::spawn(move || request(addr, &get_pot_request()));
    while svc.started.load(Ordering::SeqCst) == 0 {
        thread::sleep(Duration::from_millis(10));
    }
    let t = Instant::now();
    let reply = request(addr, "GET /ping HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(t.elapsed() < Duration::from_secs(1), "ping took {:?}", t.elapsed());
    assert!(slow.join().unwrap().starts_with("HTTP/1.1 200"));
}

#[test]
fn ping_answers_while_every_mint_worker_is_busy_and_more_wait() {
    let svc = Slow::new(Duration::from_secs(4));
    let (_running, addr) = start(svc.clone());
    let busy: Vec<_> = (0..MINT_WORKERS + 1)
        .map(|_| thread::spawn(move || request(addr, &get_pot_request())))
        .collect();
    while svc.started.load(Ordering::SeqCst) < MINT_WORKERS {
        thread::sleep(Duration::from_millis(10));
    }
    for _ in 0..3 {
        let t = Instant::now();
        let reply = request(addr, "GET /ping HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
        assert!(reply.starts_with("HTTP/1."), "{reply}");
        assert_eq!(&reply[9..12], "200");
        assert!(t.elapsed() < Duration::from_secs(1), "ping took {:?}", t.elapsed());
    }
    for b in busy {
        assert!(b.join().unwrap().starts_with("HTTP/1.1 200"));
    }
}

#[test]
fn a_cross_site_request_is_refused_on_every_route() {
    let (_running, addr) = start(Slow::new(Duration::ZERO));
    for extra in ["Sec-Fetch-Site: cross-site", "Origin: http://evil.example"] {
        let ping = request(addr, &format!("GET /ping HTTP/1.1\r\nHost: 127.0.0.1\r\n{extra}\r\nConnection: close\r\n\r\n"));
        assert!(ping.starts_with("HTTP/1.1 403"), "{ping}");
        let inv = request(
            addr,
            &format!("POST /invalidate_caches HTTP/1.1\r\nHost: 127.0.0.1\r\n{extra}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
        );
        assert!(inv.starts_with("HTTP/1.1 403"), "{inv}");
    }
    let same = request(addr, "GET /ping HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nConnection: close\r\n\r\n");
    assert!(same.starts_with("HTTP/1.1 200"), "{same}");
}

#[test]
fn invalidate_answers_204_without_a_content_type() {
    let (_running, addr) = start(Slow::new(Duration::ZERO));
    let r = request(addr, "POST /invalidate_it HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    assert!(r.starts_with("HTTP/1.1 204"), "{r}");
    assert!(!r.to_ascii_lowercase().contains("content-type"), "{r}");
}

#[test]
fn a_declared_length_over_the_limit_is_413_before_the_body_is_read() {
    let (_running, addr) = start(Slow::new(Duration::ZERO));
    let r = request(
        addr,
        "POST /get_pot HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: 200000\r\nConnection: close\r\n\r\n",
    );
    assert!(r.starts_with("HTTP/1.1 413"), "{r}");
}

#[test]
fn a_chunked_body_over_the_limit_is_413() {
    let (_running, addr) = start(Slow::new(Duration::ZERO));
    let chunk = "a".repeat(10_000);
    let mut raw = String::from(
        "POST /get_pot HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
    );
    for _ in 0..15 {
        raw.push_str(&format!("{:x}\r\n{chunk}\r\n", chunk.len()));
    }
    raw.push_str("0\r\n\r\n");
    let r = request(addr, &raw);
    assert!(r.starts_with("HTTP/1.1 413"), "{r}");
}

#[test]
fn a_full_mint_queue_answers_503_and_ping_still_answers() {
    let svc = Slow::new(Duration::from_secs(1));
    let (_running, addr) = start(svc.clone());
    let n = MINT_WORKERS + MINT_QUEUE + 4;
    let all: Vec<_> = (0..n).map(|_| thread::spawn(move || request(addr, &get_pot_request()))).collect();
    while svc.started.load(Ordering::SeqCst) < MINT_WORKERS {
        thread::sleep(Duration::from_millis(10));
    }
    let t = Instant::now();
    let ping = request(addr, "GET /ping HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(ping.starts_with("HTTP/1."), "{ping}");
    assert!(t.elapsed() < Duration::from_secs(1), "ping took {:?}", t.elapsed());
    let replies: Vec<String> = all.into_iter().map(|h| h.join().unwrap()).collect();
    let ok = replies.iter().filter(|r| r.starts_with("HTTP/1.1 200")).count();
    let refused = replies.iter().filter(|r| r.starts_with("HTTP/1.1 503")).count();
    assert_eq!((ok, refused), (MINT_WORKERS + MINT_QUEUE, 4), "{replies:?}");
}

#[test]
fn a_mint_that_panics_answers_500_and_calls_the_panic_hook() {
    // Arrange
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let hook = Arc::new(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    let running = serve_with("127.0.0.1:0".parse().unwrap(), Arc::new(Panics), hook).expect("bind a free loopback port");

    // Act
    let reply = request(running.addr(), &get_pot_request());

    // Assert
    assert!(reply.starts_with("HTTP/1.1 500"), "{reply}");
    let t = Instant::now();
    while calls.load(Ordering::SeqCst) == 0 && t.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn answers_the_supervisors_http_1_0_ping() {
    let (_running, addr) = start(Slow::new(Duration::ZERO));
    let reply = request(addr, "GET /ping HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(reply.as_bytes().starts_with(b"HTTP/1."), "{reply}");
    assert_eq!(&reply[9..12], "200");
    assert!(reply.contains("\"version\":\"2.0.0\""));
}
