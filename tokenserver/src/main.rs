use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::exit;
use std::sync::Arc;
use std::time::SystemTime;

use streamloop_pot::engine::BotguardEngine;
use streamloop_pot::google::{GoogleWeb, USER_AGENT};
use streamloop_pot::http;
use streamloop_pot::session::Sessions;

struct Args {
    host: IpAddr,
    port: u16,
    data: Option<PathBuf>,
}

fn die(code: i32, message: &str) -> ! {
    eprintln!("streamloop-pot: {message}");
    exit(code);
}

fn parse_args(args: &[String]) -> Result<Args, (i32, String)> {
    if args.first().map(String::as_str) != Some("server") {
        return Err((2, "usage: streamloop-pot server --host 127.0.0.1 --port 4416 [--data DIR]".into()));
    }
    let (mut host, mut port, mut data) = (None, None, None);
    let mut it = args[1..].iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or((2, format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--host" => host = Some(value.clone()),
            "--port" => port = Some(value.clone()),
            "--data" => data = Some(PathBuf::from(value)),
            other => return Err((2, format!("unknown option {other}"))),
        }
    }
    let host = host.ok_or((2, "--host is required".to_owned()))?;
    let host = match host.as_str() {
        "127.0.0.1" | "::1" => host.parse::<IpAddr>().expect("loopback literal"),
        _ => return Err((2, "--host must be 127.0.0.1 or ::1".into())),
    };
    let port = port
        .ok_or((2, "--port is required".to_owned()))?
        .parse::<u16>()
        .map_err(|_| (2, "--port must be a number from 1 to 65535".to_owned()))?;
    Ok(Args { host, port, data })
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&argv).unwrap_or_else(|(code, message)| die(code, &message));
    if let Some(dir) = &args.data {
        if let Err(e) = std::fs::create_dir_all(dir) {
            die(1, &format!("cannot create {}: {e}", dir.display()));
        }
    }
    let web = GoogleWeb::new().unwrap_or_else(|e| die(1, &e));
    // BotGuard snapshots stay off until they are proven, so --data only has to exist.
    let engine = BotguardEngine::new(USER_AGENT, None);
    let sessions = Sessions::new(Box::new(engine), Box::new(web), Arc::new(SystemTime::now));
    let addr = SocketAddr::new(args.host, args.port);
    let running = http::serve(addr, Arc::new(sessions))
        .unwrap_or_else(|e| die(2, &format!("cannot listen on {addr}: {e}")));
    println!("streamloop-pot {} listening on {}", env!("CARGO_PKG_VERSION"), running.addr());
    // Runs until the supervisor's Job Object kills it.
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn a_host_other_than_loopback_exits_2_with_the_message() {
        let e = parse_args(&args("server --host 0.0.0.0 --port 1")).err().unwrap();
        assert_eq!(e, (2, "--host must be 127.0.0.1 or ::1".to_owned()));
    }

    #[test]
    fn loopback_hosts_parse() {
        assert!(parse_args(&args("server --host ::1 --port 4416 --data x")).is_ok());
        assert!(parse_args(&args("server --host 127.0.0.1 --port 4416")).is_ok());
    }
}
