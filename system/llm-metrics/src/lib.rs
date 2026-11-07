// SPDX-License-Identifier: MulanPSL-2.0
//! One JSON line per model request, for usage metrics and later billing.
//!
//! Rust counterpart of `robonix_api.llm.metrics`: the record and the usage
//! parsing are the same, and both run `contract/usage_cases.json`. Records go
//! to `$ROBONIX_LLM_METRICS_DIR/<consumer>-<pid>.jsonl`; nothing is written
//! when the variable is unset.

use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

/// Record layout version; bump it when a field changes meaning.
pub const SCHEMA_VERSION: u32 = 1;

/// Serialises appends from concurrent requests in one process.
static WRITE_LOCK: Mutex<()> = Mutex::new(());
/// Set after the first write failure so the warning is logged once.
static WARNED: AtomicBool = AtomicBool::new(false);

/// Token counts a provider reported; `None` means it did not report that count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    /// Prompt tokens, including cached ones.
    pub input_tokens: Option<u64>,
    /// Generated tokens, including reasoning.
    pub output_tokens: Option<u64>,
    /// Prompt tokens served from the provider's cache, usually billed lower.
    pub cached_input_tokens: Option<u64>,
    /// Output tokens spent on hidden reasoning.
    pub reasoning_tokens: Option<u64>,
    /// Model the response names; behind a relay this is the model actually used.
    pub provider_model: Option<String>,
}

impl Usage {
    /// Read usage from a Chat Completions body or stream chunk.
    #[must_use]
    pub fn from_response(body: &Value) -> Self {
        let usage = body.get("usage").filter(|usage| usage.is_object());
        let count = |value: Option<&Value>| value.and_then(Value::as_u64);
        let field = |name: &str| usage.and_then(|usage| usage.get(name));
        let detail = |group: &str, name: &str| count(field(group).and_then(|g| g.get(name)));
        Self {
            input_tokens: count(field("prompt_tokens")),
            output_tokens: count(field("completion_tokens")),
            // DeepSeek reports cache hits outside prompt_tokens_details.
            cached_input_tokens: detail("prompt_tokens_details", "cached_tokens")
                .or_else(|| count(field("prompt_cache_hit_tokens"))),
            reasoning_tokens: detail("completion_tokens_details", "reasoning_tokens"),
            provider_model: body
                .get("model")
                .and_then(Value::as_str)
                .filter(|model| !model.is_empty())
                .map(str::to_owned),
        }
    }

    /// Keep the counts this chunk reports; streams send usage in a late chunk.
    pub fn merge(&mut self, other: Self) {
        self.input_tokens = other.input_tokens.or(self.input_tokens);
        self.output_tokens = other.output_tokens.or(self.output_tokens);
        self.cached_input_tokens = other.cached_input_tokens.or(self.cached_input_tokens);
        self.reasoning_tokens = other.reasoning_tokens.or(self.reasoning_tokens);
        self.provider_model = other.provider_model.or(self.provider_model.take());
    }
}

/// How a request ended, as recorded in `status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The provider answered with a success status.
    Ok,
    /// The provider answered with HTTP 4xx/5xx.
    HttpError,
    /// The request did not complete in time.
    Timeout,
    /// The caller abandoned the request.
    Cancelled,
    /// The connection failed or the reply could not be read.
    TransportError,
}

/// One model request, written when it is dropped.
///
/// A call that is never finished is recorded as `cancelled`, which is what
/// dropping an in-flight future means.
#[derive(Debug)]
pub struct Call {
    /// Fields written to the record.
    record: Record,
    /// When the request was sent, for `latency_ms`.
    started: Instant,
}

/// The JSON line for one request; field names match the Python writer.
#[derive(Debug, Serialize)]
struct Record {
    /// Layout version, [`SCHEMA_VERSION`].
    v: u32,
    /// Component that made the request, such as `pilot`.
    consumer: String,
    /// What the component used the model for, such as `plan`.
    operation: String,
    /// Endpoint host and port only; the path and credentials are dropped.
    host: String,
    /// Model name sent in the request.
    model: String,
    /// Catalog entry the endpoint came from, when one was used.
    catalog_id: Option<String>,
    /// Whether the reply was streamed.
    stream: bool,
    /// Outcome of the request.
    status: Status,
    /// HTTP status, when a response arrived.
    http_status: Option<u16>,
    /// Requests sent, counting retries; each may be billed.
    attempts: u32,
    /// Reported token counts.
    #[serde(flatten)]
    usage: Usage,
}

impl Call {
    /// Start timing a request to `base_url` with `model`.
    #[must_use]
    pub fn start(
        consumer: &str,
        operation: &str,
        base_url: &str,
        model: &str,
        stream: bool,
        catalog_id: Option<&str>,
    ) -> Self {
        Self {
            record: Record {
                v: SCHEMA_VERSION,
                consumer: consumer.to_owned(),
                operation: operation.to_owned(),
                host: host_of(base_url),
                model: model.to_owned(),
                catalog_id: catalog_id.map(str::to_owned),
                stream,
                status: Status::Cancelled,
                http_status: None,
                attempts: 1,
                usage: Usage::default(),
            },
            started: Instant::now(),
        }
    }

    /// Count one more request sent for this call, such as a retry.
    pub fn retry(&mut self) {
        self.record.attempts += 1;
    }

    /// Record usage from a body or stream chunk.
    pub fn observe(&mut self, body: &Value) {
        self.record.usage.merge(Usage::from_response(body));
    }

    /// Set the outcome; the record is written when the call is dropped.
    pub fn finish(&mut self, status: Status, http_status: Option<u16>) {
        self.record.status = status;
        self.record.http_status = http_status;
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        let Ok(mut line) = serde_json::to_value(&self.record) else {
            return;
        };
        let latency_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            });
        let boot_id = std::env::var("RBNX_BOOT_ID")
            .ok()
            .filter(|id| !id.is_empty());
        line["latency_ms"] = latency_ms.into();
        line["ts_ms"] = ts_ms.into();
        line["boot_id"] = boot_id.into();
        line["pid"] = std::process::id().into();
        write(&self.record.consumer, &line);
    }
}

/// Endpoint host with an explicit port, without scheme, credentials or path.
fn host_of(base_url: &str) -> String {
    let rest = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    host.to_ascii_lowercase()
}

/// Append one line; a metrics failure must not fail the model call.
fn write(consumer: &str, line: &Value) {
    let Some(directory) = std::env::var_os("ROBONIX_LLM_METRICS_DIR").filter(|d| !d.is_empty())
    else {
        return;
    };
    let path =
        std::path::Path::new(&directory).join(format!("{consumer}-{}.jsonl", std::process::id()));
    let _guard = WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let result = std::fs::create_dir_all(&directory).and_then(|()| {
        let mut sink = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(sink, "{line}")
    });
    if let Err(error) = result
        && !WARNED.swap(true, Ordering::Relaxed)
    {
        robonix_scribe::warn!("llm metrics: cannot write {}: {error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::host_of;

    #[test]
    fn host_drops_scheme_credentials_and_path() {
        assert_eq!(
            host_of("https://u:k@API.example.com/v1?key=s"),
            "api.example.com"
        );
        assert_eq!(host_of("http://127.0.0.1:8000/v1"), "127.0.0.1:8000");
        assert_eq!(host_of(""), "");
    }
}
