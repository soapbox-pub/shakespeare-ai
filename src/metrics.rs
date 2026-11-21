//! A few Prometheus counters, in the text exposition format, with the names
//! the old service used.

use std::collections::BTreeMap;
use std::fmt::Write;

use parking_lot::Mutex;

use crate::money::Money;

#[derive(Default)]
pub struct Metrics {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// (method, path, status) → count.
    responses: BTreeMap<(String, String, u16), u64>,
    prompt_tokens: BTreeMap<String, u64>,
    completion_tokens: BTreeMap<String, u64>,
    cost: BTreeMap<String, i64>,
    /// Upstream (`provider/model`) → failures that sent a request on to
    /// the next.
    upstream_failures: BTreeMap<String, u64>,
}

impl Metrics {
    pub fn response(&self, method: &str, path: &str, status: u16) {
        *self.inner.lock().responses.entry((method.into(), path.into(), status)).or_default() += 1;
    }

    pub fn usage(&self, model: &str, prompt: u64, completion: u64, cost: Money) {
        let mut inner = self.inner.lock();
        *inner.prompt_tokens.entry(model.into()).or_default() += prompt;
        *inner.completion_tokens.entry(model.into()).or_default() += completion;
        *inner.cost.entry(model.into()).or_default() += cost.nanos();
    }

    pub fn upstream_failure(&self, upstream: &str) {
        *self.inner.lock().upstream_failures.entry(upstream.into()).or_default() += 1;
    }

    pub fn render(&self) -> String {
        let inner = self.inner.lock();
        let mut out = String::new();
        let _ = writeln!(out, "# TYPE shakespeare_http_responses_total counter");
        for ((method, path, status), n) in &inner.responses {
            let _ = writeln!(out, "shakespeare_http_responses_total{{method=\"{method}\",path=\"{}\",status=\"{status}\"}} {n}", escape(path));
        }
        for (name, map) in [("prompt_tokens_total", &inner.prompt_tokens), ("completion_tokens_total", &inner.completion_tokens)] {
            let _ = writeln!(out, "# TYPE shakespeare_{name} counter");
            for (model, n) in map {
                let _ = writeln!(out, "shakespeare_{name}{{model=\"{}\"}} {n}", escape(model));
            }
        }
        let _ = writeln!(out, "# TYPE shakespeare_cost_usd_total counter");
        for (model, nanos) in &inner.cost {
            let _ = writeln!(out, "shakespeare_cost_usd_total{{model=\"{}\"}} {}", escape(model), Money(*nanos).to_f64());
        }
        let _ = writeln!(out, "# TYPE shakespeare_upstream_failures_total counter");
        for (upstream, n) in &inner.upstream_failures {
            let _ = writeln!(out, "shakespeare_upstream_failures_total{{upstream=\"{}\"}} {n}", escape(upstream));
        }
        out
    }
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}
