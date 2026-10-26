//! HTTP API on 127.0.0.1. Requests become messages to the control loop, except GET /status,
//! which reads a snapshot the loop refreshes so 2.x's 1 s check never waits on a busy loop.

use crate::Msg;
use serde_json::{json, Value};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

/// Latest status JSON, replaced by the control loop after every tick and handled message.
pub type Snapshot = Arc<Mutex<Value>>;

fn respond(req: Request, code: u16, body: Value) {
    let header = Header::from_bytes("Content-Type", "application/json").unwrap();
    let _ = req.respond(Response::from_string(body.to_string()).with_status_code(code).with_header(header));
}

fn ask<T>(tx: &Sender<Msg>, make: impl FnOnce(Sender<T>) -> Msg) -> Option<T> {
    let (reply, rx) = channel();
    tx.send(make(reply)).ok()?;
    rx.recv_timeout(Duration::from_secs(10)).ok()
}

pub fn serve(port: u16, debug_faults: bool, tx: Sender<Msg>, snapshot: Snapshot) -> Result<(), String> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| format!("bind 127.0.0.1:{port}: {e}"))?;
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            // One thread per request: a slow body or a request waiting on the loop must not delay GET /status.
            let (tx, snapshot) = (tx.clone(), snapshot.clone());
            std::thread::spawn(move || {
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let parsed = serde_json::from_str::<Value>(if body.trim().is_empty() { "{}" } else { &body });
                let (method, url) = (req.method().clone(), req.url().to_string());
                let result: Option<Result<Value, String>> = match (method, url.as_str()) {
                    (Method::Get, "/status") => Some(Ok(snapshot.lock().unwrap().clone())),
                    (Method::Post, "/control") => match parsed {
                        Err(e) => Some(Err(format!("bad JSON: {e}"))),
                        Ok(v) => {
                            let cmd = v["cmd"].as_str().unwrap_or("").to_string();
                            let index = v["index"].as_i64();
                            // restart-encoder's note for the event log, capped at 200 characters
                            let reason = v["reason"].as_str().map(|r| r.chars().take(200).collect());
                            let id = v["id"].as_str().map(String::from);
                            if !v["index"].is_null() && index.is_none() {
                                Some(Err("index must be an integer".into()))
                            } else if !v["id"].is_null() && id.is_none() {
                                Some(Err("id must be a string".into()))
                            } else {
                                ask(&tx, |reply| Msg::Control { cmd, index, id, reason, reply })
                            }
                        }
                    },
                    (Method::Post, "/reload") => match parsed {
                        Err(e) => Some(Err(format!("bad JSON: {e}"))),
                        Ok(v) => {
                            let index = v["index"].as_u64().map(|i| i as usize);
                            let id = v["id"].as_str().map(String::from);
                            if !v["index"].is_null() && index.is_none() {
                                Some(Err("index must be a non-negative integer".into()))
                            } else if !v["id"].is_null() && id.is_none() {
                                Some(Err("id must be a string".into()))
                            } else {
                                ask(&tx, |reply| Msg::Reload { index, id, reply })
                            }
                        }
                    },
                    (Method::Post, "/debug/fault") if debug_faults => match parsed {
                        Err(e) => Some(Err(format!("bad JSON: {e}"))),
                        Ok(v) => {
                            let kind = v["kind"].as_str().unwrap_or("").to_string();
                            // "always" (or no count) breaks every resolve of the video.
                            let count = v["count"].as_u64().map(|c| c.max(1) as u32);
                            ask(&tx, |reply| Msg::Fault { kind, count, reply })
                        }
                    },
                    (Method::Post, "/shutdown") => {
                        respond(req, 200, json!({"accepted": true}));
                        let _ = tx.send(Msg::Shutdown);
                        return;
                    }
                    _ => {
                        respond(req, 404, json!({"error": "not found"}));
                        return;
                    }
                };
                match result {
                    Some(Ok(v)) => respond(req, 200, v),
                    Some(Err(e)) => respond(req, 400, json!({"error": e})),
                    None => respond(req, 503, json!({"error": "control loop did not answer"})),
                }
            });
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::Instant;

    fn get_status(port: u16) -> (String, Duration) {
        let started = Instant::now();
        let mut conn = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        conn.write_all(b"GET /status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
        let mut reply = String::new();
        let mut buf = [0u8; 1024];
        while !reply.ends_with('}') {
            let n = conn.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            reply.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        (reply, started.elapsed())
    }

    fn start() -> (u16, Sender<Msg>, std::sync::mpsc::Receiver<Msg>) {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (tx, rx) = channel::<Msg>();
        let snapshot: Snapshot = Arc::new(Mutex::new(json!({"state": "starting", "encoder": {"alive": false}})));
        serve(port, false, tx.clone(), snapshot).unwrap();
        (port, tx, rx)
    }

    #[test]
    fn status_answers_within_a_second_while_a_control_request_waits_on_the_loop() {
        // Arrange: a POST /control is parked on the channel nobody reads
        let (port, _tx, _rx) = start();
        std::thread::spawn(move || {
            let mut conn = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            let body = r#"{"cmd":"next"}"#;
            let _ = conn.write_all(format!("POST /control HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
            let mut sink = Vec::new();
            let _ = conn.read_to_end(&mut sink);
        });
        std::thread::sleep(Duration::from_millis(300));

        // Act
        let (reply, took) = get_status(port);

        // Assert
        assert!(took < Duration::from_secs(1), "took {took:?}");
        assert!(reply.contains("\"state\":\"starting\""), "{reply}");
    }

    #[test]
    fn status_answers_within_a_second_while_the_control_loop_is_blocked() {
        // Arrange: nobody reads the control channel
        let (port, _tx, _rx) = start();

        // Act
        let (reply, took) = get_status(port);

        // Assert
        assert!(took < Duration::from_secs(1), "took {took:?}");
        assert!(reply.contains("\"alive\":false") && reply.contains("\"state\":\"starting\""), "{reply}");
    }

    fn post(port: u16, path: &str, body: &str) -> String {
        let mut conn = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        conn.write_all(format!("POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
        let mut reply = String::new();
        let _ = conn.read_to_string(&mut reply);
        reply
    }

    /// Answers every reload and control on the channel and hands back the index and id each carried.
    fn reload_indexes(rx: std::sync::mpsc::Receiver<Msg>) -> std::sync::mpsc::Receiver<(Option<i64>, Option<String>)> {
        let (seen, out) = channel();
        std::thread::spawn(move || {
            for msg in rx {
                let (got, reply) = match msg {
                    Msg::Reload { index, id, reply } => ((index.map(|i| i as i64), id), reply),
                    Msg::Control { index, id, reply, .. } => ((index, id), reply),
                    _ => continue,
                };
                let _ = seen.send(got);
                let _ = reply.send(Ok(json!({"applied": true})));
            }
        });
        out
    }

    #[test]
    fn reload_passes_an_optional_start_index_and_keeps_an_empty_body_working() {
        let (port, _tx, rx) = start();
        let seen = reload_indexes(rx);

        let replies = [
            post(port, "/reload", ""),
            post(port, "/reload", "{}"),
            post(port, "/reload", r#"{"index":3}"#),
            post(port, "/reload", r#"{"index":3,"id":"abc"}"#),
            post(port, "/control", r#"{"cmd":"jump","index":2,"id":"xyz"}"#),
        ];

        assert!(replies.iter().all(|r| r.starts_with("HTTP/1.1 200")), "{replies:?}");
        let got: Vec<_> = (0..5).map(|_| seen.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        assert_eq!(got, [(None, None), (None, None), (Some(3), None), (Some(3), Some("abc".into())), (Some(2), Some("xyz".into()))]);
    }

    #[test]
    fn reload_rejects_an_index_that_is_not_a_non_negative_integer() {
        let (port, _tx, rx) = start();
        let seen = reload_indexes(rx);

        for (path, body) in [("/reload", r#"{"index":-1}"#), ("/reload", r#"{"index":1.5}"#), ("/reload", r#"{"index":"2"}"#), ("/reload", r#"{"id":7}"#), ("/control", r#"{"cmd":"jump","index":1,"id":7}"#)] {
            let reply = post(port, path, body);
            assert!(reply.starts_with("HTTP/1.1 400"), "{body}: {reply}");
        }
        assert!(seen.recv_timeout(Duration::from_millis(300)).is_err(), "a bad index reached the loop");
    }
}
