use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use streamloop_pot::http::{serve, serve_with, Service, MINT_QUEUE, MINT_WORKERS};
use streamloop_pot::session::{PotAnswer, PotRequest};

fn answer(r: &PotRequest) -> PotAnswer {
    PotAnswer {
        content_binding: r.content_binding.clone(),
        po_token: "tok".into(),
        expires_at: UNIX_EPOCH + Duration::from_secs(1_794_139_200),
    }
}

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
        Ok(answer(r))
    }
    fn invalidate_caches(&self) {}
    fn invalidate_it(&self) {}
    fn minter_keys(&self) -> Vec<String> {
        vec![]
    }
}

/// Holds every mint until `release`, so a worker frees nothing however slowly the requests arrive.
struct Held {
    released: Mutex<bool>,
    release: Condvar,
    started: AtomicUsize,
}

impl Held {
    fn new() -> Arc<Self> {
        Arc::new(Self { released: Mutex::new(false), release: Condvar::new(), started: AtomicUsize::new(0) })
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.release.notify_all();
    }
}

impl Service for Held {
    fn get_pot(&self, r: &PotRequest) -> Result<PotAnswer, String> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.release.wait(released).unwrap();
        }
        Ok(answer(r))
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

fn start(svc: Arc<dyn Service>) -> (streamloop_pot::http::Running, SocketAddr) {
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

/// One reply read off a kept-alive connection: its head and the body its Content-Length gives.
fn read_reply(s: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let length = head
        .lines()
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
        .unwrap_or(0);
    let mut body = vec![0; length];
    s.read_exact(&mut body).unwrap();
    head + &String::from_utf8_lossy(&body)
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

/// The largest single allocation this test binary has made, counted from 1 MiB up.
static LARGEST_ALLOC: AtomicUsize = AtomicUsize::new(0);

struct Tracking;

fn note(size: usize) {
    if size >= 1 << 20 {
        LARGEST_ALLOC.fetch_max(size, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        System.alloc(layout)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        System.alloc_zeroed(layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        System.realloc(ptr, layout, new_size)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static TRACKING: Tracking = Tracking;

#[test]
fn refuses_a_huge_declared_body_without_reading_or_allocating_it() {
    // Arrange: a /get_pot with an Origin, one cross-site and one over the limit, each declaring 256 MiB it never sends
    let (_running, addr) = start(Slow::new(Duration::ZERO));
    let declared: usize = 256 << 20;
    let cases = [("Origin: http://evil.example\r\n", "403"), ("Sec-Fetch-Site: cross-site\r\n", "403"), ("", "413")];

    for (extra, status) in cases {
        let mut conn = TcpStream::connect(addr).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let head = format!(
            "POST /get_pot HTTP/1.1\r\nHost: 127.0.0.1\r\n{extra}Content-Type: application/json\r\nContent-Length: {declared}\r\n\r\n"
        );
        conn.write_all(head.as_bytes()).unwrap();

        // Act: read until the token server closes the connection, or for 3 s
        let mut reply = Vec::new();
        let closed = conn.read_to_end(&mut reply).is_ok();

        // Assert
        let reply = String::from_utf8_lossy(&reply);
        assert!(reply.starts_with(&format!("HTTP/1.1 {status}")), "{extra:?}: {reply}");
        let largest = LARGEST_ALLOC.load(Ordering::Relaxed);
        assert!(largest < declared / 2, "{extra:?}: the token server allocated {largest} bytes for a body it refused");
        assert!(closed, "{extra:?}: the connection was still open 3 s after the {status}, waiting for the body");
    }
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
    // Arrange: a held mint on every worker, so only the queue has room left
    let svc = Held::new();
    let (_running, addr) = start(svc.clone());
    let (reply_tx, replies) = mpsc::channel();
    let mint = |count: usize| {
        for _ in 0..count {
            let tx = reply_tx.clone();
            thread::spawn(move || {
                let _ = tx.send(request(addr, &get_pot_request()));
            });
        }
    };
    mint(MINT_WORKERS);
    while svc.started.load(Ordering::SeqCst) < MINT_WORKERS {
        thread::sleep(Duration::from_millis(10));
    }
    let past_the_queue = 4;
    let next_reply = || replies.recv_timeout(Duration::from_secs(15)).expect("a reply within 15 s");

    // Act: no mint ends while they are held, so only refusals come back, and once the 4th has,
    // every request has been queued or refused
    mint(MINT_QUEUE + past_the_queue);
    let refused: Vec<String> = (0..past_the_queue).map(|_| next_reply()).collect();
    let ping = request(addr, "GET /ping HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
    svc.release();
    let minted: Vec<String> = (0..MINT_WORKERS + MINT_QUEUE).map(|_| next_reply()).collect();

    // Assert
    assert!(refused.iter().all(|r| r.starts_with("HTTP/1.1 503")), "{refused:?}");
    assert!(ping.starts_with("HTTP/1.") && ping.get(9..12) == Some("200"), "{ping}");
    assert!(minted.iter().all(|r| r.starts_with("HTTP/1.1 200")), "{minted:?}");
}

#[test]
fn answers_mints_and_a_ping_one_after_another_on_a_kept_alive_connection() {
    // Arrange: a body over 1 KiB, which tiny_http reads as the server asks for it rather than with the head
    let (_running, addr) = start(Slow::new(Duration::ZERO));
    let body = format!(r#"{{"content_binding":"vd","innertube_context":{{"client":{{"pad":"{}"}}}}}}"#, "a".repeat(4096));
    let mint = format!(
        "POST /get_pot HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut conn = TcpStream::connect(addr).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(15))).unwrap();

    // Act
    let mut replies = Vec::new();
    for raw in [mint.as_str(), mint.as_str(), "GET /ping HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"] {
        conn.write_all(raw.as_bytes()).unwrap();
        replies.push(read_reply(&mut conn));
    }

    // Assert
    for mint in &replies[..2] {
        assert!(mint.starts_with("HTTP/1.1 200") && mint.contains(r#""poToken":"tok""#), "{mint}");
    }
    assert!(replies[2].starts_with("HTTP/1.1 200") && replies[2].contains(r#""version":"2.0.0""#), "{}", replies[2]);
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
