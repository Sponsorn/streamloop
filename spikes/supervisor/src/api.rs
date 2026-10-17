//! HTTP API on 127.0.0.1. Every request becomes a message to the control loop.

use crate::Msg;
use serde_json::{json, Value};
use std::sync::mpsc::{channel, Sender};
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

fn respond(req: Request, code: u16, body: Value) {
    let header = Header::from_bytes("Content-Type", "application/json").unwrap();
    let _ = req.respond(Response::from_string(body.to_string()).with_status_code(code).with_header(header));
}

fn ask<T>(tx: &Sender<Msg>, make: impl FnOnce(Sender<T>) -> Msg) -> Option<T> {
    let (reply, rx) = channel();
    tx.send(make(reply)).ok()?;
    rx.recv_timeout(Duration::from_secs(10)).ok()
}

pub fn serve(port: u16, debug_faults: bool, tx: Sender<Msg>) -> Result<(), String> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| format!("bind 127.0.0.1:{port}: {e}"))?;
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let parsed = serde_json::from_str::<Value>(if body.trim().is_empty() { "{}" } else { &body });
            let (method, url) = (req.method().clone(), req.url().to_string());
            let result: Option<Result<Value, String>> = match (method, url.as_str()) {
                (Method::Get, "/status") => ask(&tx, Msg::Status).map(Ok),
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
