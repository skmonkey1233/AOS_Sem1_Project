//! Small, dependency-free HTTP adapter for the existing scheduler engine.
//! The browser talks to this local server; all scheduling stays in Rust.

use sjs::engine::{simulate_with_dependencies, Kind, SimResult};
use sjs::metrics::{analyze, ProcMetrics, Summary};
use sjs::policy::{Algo, Config};
use sjs::smart::{benchmark_with_dependencies, heuristic, profile, Goal, Ranked};
use sjs::workload;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

const MAX_REQUEST: usize = 2 * 1024 * 1024;

pub fn serve(port: u16) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("could not listen on 127.0.0.1:{}: {}", port, e))?;
    println!(
        "Smart Job Scheduler is available at http://127.0.0.1:{}/",
        port
    );
    println!("Press Ctrl+C to stop the local server.");
    for incoming in listener.incoming() {
        match incoming {
            Ok(mut stream) => {
                if let Err(e) = handle(&mut stream) {
                    eprintln!("request error: {}", e);
                }
            }
            Err(e) => eprintln!("connection error: {}", e),
        }
    }
    Ok(())
}

fn handle(stream: &mut TcpStream) -> Result<(), String> {
    let (method, path, body) = read_request(stream).map_err(|e| e.to_string())?;
    let (status, content_type, payload) = if method == "GET" {
        serve_file(&path)
    } else if method == "POST" && path.starts_with("/api/") {
        match api(&path, &body) {
            Ok(json) => (200, "application/json; charset=utf-8", json.into_bytes()),
            Err(message) => (
                400,
                "application/json; charset=utf-8",
                error_json(&message).into_bytes(),
            ),
        }
    } else {
        (404, "text/plain; charset=utf-8", b"Not found".to_vec())
    };
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\r\n",
        status, reason, content_type, payload.len()
    );
    stream
        .write_all(header.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(&payload).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, String, Vec<u8>)> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut header_end = None;
    let mut content_length = 0usize;
    loop {
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > MAX_REQUEST {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request is too large",
            ));
        }
        if header_end.is_none() {
            if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = Some(pos);
                let headers = String::from_utf8_lossy(&bytes[..pos]);
                content_length = headers
                    .lines()
                    .skip(1)
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if name.eq_ignore_ascii_case("content-length") {
                            value.trim().parse().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                if content_length > MAX_REQUEST {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "request body is too large",
                    ));
                }
            }
        }
        if let Some(pos) = header_end {
            if bytes.len() >= pos + 4 + content_length {
                break;
            }
        }
    }
    let pos = header_end.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "missing HTTP headers")
    })?;
    let headers = String::from_utf8_lossy(&bytes[..pos]);
    let mut first = headers.lines().next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("").to_string();
    let path = first
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    let body = bytes[(pos + 4)..(pos + 4 + content_length).min(bytes.len())].to_vec();
    Ok((method, path, body))
}

fn serve_file(path: &str) -> (u16, &'static str, Vec<u8>) {
    let (name, kind) = match path {
        "/" | "/index.html" => ("index.html", "text/html; charset=utf-8"),
        "/styles.css" => ("styles.css", "text/css; charset=utf-8"),
        "/app.js" => ("app.js", "text/javascript; charset=utf-8"),
        _ => return (404, "text/plain; charset=utf-8", b"Not found".to_vec()),
    };
    match std::fs::read(PathBuf::from(name)) {
        Ok(data) => (200, kind, data),
        Err(e) => (
            404,
            "text/plain; charset=utf-8",
            format!("Could not read {}: {}", name, e).into_bytes(),
        ),
    }
}

fn api(path: &str, body: &[u8]) -> Result<String, String> {
    let form = parse_form(&String::from_utf8_lossy(body));
    let csv = form.get("jobs").ok_or("The workload was missing.")?;
    let jobs = workload::parse_csv(csv)?;
    let dependencies = read_dependencies(form.get("dependencies"), jobs.len())?;
    let cfg = read_config(&form)?;
    let cs = number::<u32>(&form, "cs", 0)?;
    match path {
        "/api/simulate" => {
            let id = form.get("algo").map(String::as_str).unwrap_or("rr");
            let algo = Algo::parse(id).ok_or_else(|| format!("Unknown algorithm '{}'.", id))?;
            let mut policy = algo.build(&cfg);
            let result = simulate_with_dependencies(&jobs, &dependencies, policy.as_mut(), cs)?;
            let (rows, summary) = analyze(&result);
            Ok(result_json(&result, &rows, &summary))
        }
        "/api/compare" | "/api/smart" => {
            let goal = match form.get("goal").map(String::as_str).unwrap_or("balanced") {
                "balanced" => Goal::Balanced,
                "throughput" => Goal::Throughput,
                "interactive" => Goal::Interactive,
                "fair" => Goal::Fair,
                other => return Err(format!("Unknown optimization goal '{}'.", other)),
            };
            let ranked = benchmark_with_dependencies(&jobs, &dependencies, &cfg, cs, goal)?;
            if path == "/api/compare" {
                return Ok(ranked_json(&ranked));
            }
            let prof = profile(&jobs);
            let (suggestion, reason) = heuristic(&prof, goal);
            let best = ranked.first().ok_or("No eligible scheduling policies.")?;
            Ok(format!(
                "{{\"goal\":{},\"suggested\":{},\"suggestion_reason\":{},\"winner\":{},\"winner_score\":{},\"profile\":{{\"jobs\":{},\"mean_burst\":{},\"burst_cv\":{},\"short_ratio\":{},\"priority_levels\":{},\"arrival_span\":{}}},\"ranked\":{}}}",
                json_string(goal.name()), json_string(suggestion.id()), json_string(&reason),
                json_string(best.algo.id()), float(best.score), prof.jobs, float(prof.mean_burst),
                float(prof.burst_cv), float(prof.short_ratio), prof.priority_levels,
                prof.arrival_span, ranked_json_array(&ranked)
            ))
        }
        _ => Err("Unknown API route.".into()),
    }
}

fn read_dependencies(raw: Option<&String>, count: usize) -> Result<Vec<Option<usize>>, String> {
    let Some(raw) = raw else {
        return Ok(vec![None; count]);
    };
    if raw.trim().is_empty() {
        return Ok(vec![None; count]);
    }
    let dependencies: Result<Vec<Option<usize>>, String> = raw
        .split(',')
        .map(|value| {
            let value = value.trim();
            if value.is_empty() || value.eq_ignore_ascii_case("none") || value == "-" {
                Ok(None)
            } else {
                value
                    .parse::<usize>()
                    .map(Some)
                    .map_err(|_| "Dependency entries must be process numbers or none.".into())
            }
        })
        .collect();
    let dependencies = dependencies?;
    if dependencies.len() != count {
        return Err("Provide one dependency entry per process.".into());
    }
    Ok(dependencies)
}

fn read_config(form: &HashMap<String, String>) -> Result<Config, String> {
    let mut cfg = Config::default();
    cfg.quantum = number(form, "quantum", cfg.quantum)?.max(1);
    cfg.boost = number(form, "boost", cfg.boost)?;
    cfg.age_every = number(form, "age", cfg.age_every)?.max(1);
    if let Some(levels) = form.get("mlfq") {
        let parsed: Result<Vec<u32>, _> =
            levels.split(',').map(|s| s.trim().parse::<u32>()).collect();
        let values = parsed.map_err(|_| "MLFQ levels should look like 2,4,8.")?;
        if values.is_empty() || values.contains(&0) {
            return Err("MLFQ quanta must be positive.".into());
        }
        cfg.mlfq_quanta = values;
    }
    Ok(cfg)
}

fn number<T: std::str::FromStr>(
    form: &HashMap<String, String>,
    key: &str,
    default: T,
) -> Result<T, String> {
    match form.get(key) {
        Some(value) => value
            .parse()
            .map_err(|_| format!("'{}' must be a number.", key)),
        None => Ok(default),
    }
}

fn parse_form(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((url_decode(key), url_decode(value)))
        })
        .collect()
}

fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                decoded.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    decoded.push(byte);
                    i += 3;
                } else {
                    decoded.push(bytes[i]);
                    i += 1;
                }
            }
            byte => {
                decoded.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn result_json(result: &SimResult, rows: &[ProcMetrics], summary: &Summary) -> String {
    let jobs = rows.iter().map(|m| format!(
        "{{\"name\":{},\"arrival\":{},\"burst\":{},\"priority\":{},\"first_run\":{},\"completion\":{},\"turnaround\":{},\"waiting\":{},\"response\":{}}}",
        json_string(&m.name), m.arrival, m.burst, m.priority, m.first_run, m.completion,
        m.turnaround, m.waiting, m.response
    )).collect::<Vec<_>>().join(",");
    let timeline = result
        .timeline
        .iter()
        .map(|s| {
            let (kind, pid, name) = match s.kind {
                Kind::Run(pid) => (
                    "run",
                    pid.to_string(),
                    json_string(&result.procs[pid].job.name),
                ),
                Kind::Idle => ("idle", "null".into(), "null".into()),
                Kind::Switch => ("switch", "null".into(), "null".into()),
            };
            format!(
                "{{\"kind\":{},\"pid\":{},\"job\":{},\"start\":{},\"end\":{}}}",
                json_string(kind),
                pid,
                name,
                s.start,
                s.end
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"policy\":{},\"makespan\":{},\"busy\":{},\"overhead\":{},\"context_switches\":{},\"metrics\":{},\"jobs\":[{}],\"timeline\":[{}]}}",
        json_string(&result.policy), result.makespan, result.busy, result.overhead,
        result.context_switches, summary_json(summary), jobs, timeline
    )
}

fn ranked_json(ranked: &[Ranked]) -> String {
    format!("{{\"ranked\":{}}}", ranked_json_array(ranked))
}

fn ranked_json_array(ranked: &[Ranked]) -> String {
    let items = ranked
        .iter()
        .map(|r| {
            format!(
                "{{\"algo\":{},\"score\":{},\"metrics\":{}}}",
                json_string(r.algo.id()),
                float(r.score),
                summary_json(&r.summary)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{}]", items)
}

fn summary_json(s: &Summary) -> String {
    format!("{{\"avg_wait\":{},\"avg_tat\":{},\"avg_resp\":{},\"max_wait\":{},\"makespan\":{},\"cpu_util\":{},\"throughput\":{},\"context_switches\":{},\"overhead\":{},\"jain\":{}}}",
        float(s.avg_wait), float(s.avg_tat), float(s.avg_resp), s.max_wait, s.makespan,
        float(s.cpu_util), float(s.throughput), s.context_switches, s.overhead, float(s.jain)
    )
}

fn float(value: f64) -> String {
    if value.is_finite() {
        format!("{:.6}", value)
    } else {
        "0".into()
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn error_json(message: &str) -> String {
    format!("{{\"error\":{}}}", json_string(message))
}
