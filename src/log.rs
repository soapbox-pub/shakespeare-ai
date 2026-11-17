//! One flat JSON object per line on stdout: `{"level", "ns", ...fields}`, the
//! shape the old service's logs had.

use std::io::Write;

use serde_json::{Map, Value};

fn log(level: &str, ns: &str, fields: Value) {
    let mut line = Map::new();
    line.insert("level".into(), level.into());
    line.insert("ns".into(), ns.into());
    if let Value::Object(fields) = fields {
        line.extend(fields.into_iter().filter(|(_, v)| !v.is_null()));
    }
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut out, &line);
    let _ = out.write_all(b"\n");
}

pub fn info(ns: &str, fields: Value) {
    log("info", ns, fields);
}

pub fn warn(ns: &str, fields: Value) {
    log("warn", ns, fields);
}

pub fn error(ns: &str, fields: Value) {
    log("error", ns, fields);
}
