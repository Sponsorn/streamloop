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
                        if !v["index"].is_null() && index.is_none() {
                            Some(Err("index must be an integer".into()))
                        } else {
                            ask(&tx, |reply| Msg::Control { cmd, index, reply })
                        }
                    }
                },
                (Method::Post, "/reload") => ask(&tx, Msg::Reload),
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
                    continue;
                }
                _ => {
                    respond(req, 404, json!({"error": "not found"}));
                    continue;
                }
            };
            match result {
                Some(Ok(v)) => respond(req, 200, v),
                Some(Err(e)) => respond(req, 400, json!({"error": e})),
                None => respond(req, 503, json!({"error": "control loop did not answer"})),
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::Instant;

    #[test]
    fn status_answers_within_a_second_while_the_control_loop_is_blocked() {
        // Arrange: nobody reads the control channel
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (tx, _rx) = channel::<Msg>();
        let snapshot: Snapshot = Arc::new(Mutex::new(json!({"state": "starting", "encoder": {"alive": false}})));
        serve(port, false, tx, snapshot).unwrap();

        // Act
        let started = Instant::now();
        let mut conn = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        conn.write_all(b"GET /status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
        let mut reply = String::new();
        let mut buf = [0u8; 1024];
        while !reply.ends_with('}') {
            let n = conn.read(&mut buf).unwrap();
            reply.push_str(&String::from_utf8_lossy(&buf[..n]));
        }

        // Assert
        assert!(started.elapsed() < Duration::from_secs(1), "took {:?}", started.elapsed());
        assert!(reply.contains("\"alive\":false") && reply.contains("\"state\":\"starting\""), "{reply}");
    }
}
