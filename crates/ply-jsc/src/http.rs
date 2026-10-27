//! Native HTTP for model streams and plugin JSON APIs. SSE responses stay
//! in Rust and are read on demand; JSON APIs use bounded native responses.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use reqwest::{Client, Method, Request, Response};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, watch};

use crate::runtime::OpResult;

const MAX_BODY: usize = 16 << 20;
const MAX_REQUESTS: usize = 128;

struct Exchange {
    request: AsyncMutex<Option<Request>>,
    response: AsyncMutex<Option<Response>>,
    received: AtomicU64,
    cancel: watch::Sender<bool>,
}

#[derive(Default)]
pub(crate) struct Http {
    // A pool belongs to its realm/runtime, not to an earlier Tokio runtime
    // that may have already shut down in an embedding application.
    client: OnceLock<Result<Client, String>>,
    next: AtomicU64,
    exchanges: Mutex<HashMap<u64, Arc<Exchange>>>,
    json_inflight: AtomicUsize,
}

fn failure(message: impl Into<String>) -> OpResult {
    OpResult::error("ERR_PLY_HTTP", message)
}

async fn cancelled(signal: &mut watch::Receiver<bool>) {
    let _ = signal.wait_for(|aborted| *aborted).await;
}

impl Http {
    fn client(&self) -> Result<&Client, String> {
        self.client
            .get_or_init(|| {
                Client::builder()
                    .connect_timeout(Duration::from_secs(30))
                    .timeout(Duration::from_secs(600))
                    // JS follows redirects and removes cross-origin credentials.
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|error| format!("initializing HTTP: {error}"))
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    pub(crate) fn open(&self, args: Value, body: Option<Vec<u8>>) -> OpResult {
        match self.prepare(args, body) {
            Ok(request) => {
                let mut exchanges = self.exchanges.lock().expect("HTTP table poisoned");
                if exchanges.len() >= MAX_REQUESTS {
                    return failure("too many open HTTP responses in this realm");
                }
                let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
                let (cancel, _) = watch::channel(false);
                exchanges.insert(
                    id,
                    Arc::new(Exchange {
                        request: AsyncMutex::new(Some(request)),
                        response: AsyncMutex::new(None),
                        received: AtomicU64::new(0),
                        cancel,
                    }),
                );
                OpResult::Json(json!(id))
            }
            Err(message) => failure(message),
        }
    }

    fn prepare(&self, args: Value, body: Option<Vec<u8>>) -> Result<Request, String> {
        let url = args
            .get("url")
            .and_then(Value::as_str)
            .ok_or("HTTP URL must be a string")?;
        let url = reqwest::Url::parse(url).map_err(|_| "invalid HTTP URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("HTTP supports http/https URLs without embedded credentials".into());
        }
        let method = args.get("method").and_then(Value::as_str).unwrap_or("GET");
        let method = Method::from_bytes(method.as_bytes()).map_err(|_| "invalid HTTP method")?;
        if matches!(method.as_str(), "CONNECT" | "TRACE" | "TRACK") {
            return Err("HTTP method is not supported".into());
        }
        let mut request = self.client()?.request(method, url);
        let headers = args
            .get("headers")
            .and_then(Value::as_array)
            .ok_or("HTTP headers must be pairs")?;
        if headers.len() > 256 {
            return Err("too many HTTP headers".into());
        }
        for header in headers {
            let pair = header
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or("invalid HTTP header pair")?;
            let name = pair[0].as_str().ok_or("invalid HTTP header name")?;
            let value = pair[1].as_str().ok_or("invalid HTTP header value")?;
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| "invalid HTTP header name")?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| "invalid HTTP header value")?;
            request = request.header(name, value);
        }
        if let Some(body) = body {
            if body.len() > MAX_BODY {
                return Err("HTTP request exceeds 16 MiB".into());
            }
            request = request.body(body);
        }
        request.build().map_err(|_| "invalid HTTP request".into())
    }

    pub(crate) fn abort(&self, id: u64) {
        if let Some(exchange) = self
            .exchanges
            .lock()
            .expect("HTTP table poisoned")
            .remove(&id)
        {
            exchange.cancel.send_replace(true);
        }
    }

    pub(crate) fn abort_all(&self) {
        for (_, exchange) in self.exchanges.lock().expect("HTTP table poisoned").drain() {
            exchange.cancel.send_replace(true);
        }
    }

    pub(crate) async fn call(&self, op: &str, args: Value) -> OpResult {
        if op == "http.request_json" {
            return self.request_json(args).await;
        }
        let Some(id) = args.get("id").and_then(Value::as_u64) else {
            return failure("invalid HTTP handle");
        };
        let exchange = self
            .exchanges
            .lock()
            .expect("HTTP table poisoned")
            .get(&id)
            .cloned();
        let Some(exchange) = exchange else {
            return failure("HTTP request is closed or aborted");
        };
        let result = match op {
            "http.headers" => self.headers(&exchange).await,
            "http.read" => self.read(&exchange).await,
            _ => failure("unknown HTTP operation"),
        };
        if matches!(&result, OpResult::Error(_) | OpResult::Json(Value::Null)) {
            self.abort(id);
        }
        result
    }

    /// JSON APIs use the same Rust client, with host-only bearer credentials.
    /// The owning invocation's task is aborted on cancellation/realm retirement.
    async fn request_json(&self, args: Value) -> OpResult {
        let started = Instant::now();
        let timeout_ms = match args.get("timeout_ms") {
            None => 10000,
            Some(value) => match value.as_u64().filter(|n| *n > 0 && *n <= 600000) {
                Some(value) => value,
                None => return failure("JSON timeout_ms must be an integer in 1..600000"),
            },
        };
        let body = match serde_json::to_vec(args.get("body").unwrap_or(&Value::Null)) {
            Ok(body) if body.len() <= MAX_BODY => body,
            _ => return failure("invalid or oversized HTTP JSON body"),
        };
        let mut headers = vec![json!(["content-type", "application/json"])];
        if let Some(provider) = args.get("auth_provider") {
            let Some(provider) = provider.as_str() else {
                return failure("auth_provider must be a string");
            };
            match credentials(&json!({ "provider": provider })) {
                OpResult::Json(Value::String(key)) => {
                    headers.push(json!(["authorization", format!("Bearer {key}")]))
                }
                other => return other,
            }
        }
        let request = match self.prepare(
            json!({ "url": args.get("url"), "method": "POST", "headers": headers }),
            Some(body),
        ) {
            Ok(request) => request,
            Err(message) => return failure(message),
        };
        if self
            .json_inflight
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < MAX_REQUESTS).then_some(count + 1)
            })
            .is_err()
        {
            return failure("too many in-flight HTTP JSON requests");
        }
        struct Inflight<'a>(&'a AtomicUsize);
        impl Drop for Inflight<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }
        let _inflight = Inflight(&self.json_inflight);
        let request = async {
            let client = match self.client() {
                Ok(client) => client,
                Err(_) => return failure("HTTP client unavailable"),
            };
            let mut response = match client.execute(request).await {
                Ok(response) => response,
                Err(_) => return failure("HTTP JSON request failed"),
            };
            let status = response.status().as_u16();
            // Never forward error bodies, headers, or echoed credentials to JS.
            // Redirects are not followed by this client.
            if !response.status().is_success() {
                return OpResult::Json(json!({ "status": status, "data": Value::Null }));
            }
            let mut bytes = Vec::new();
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) if bytes.len().saturating_add(chunk.len()) <= MAX_BODY => {
                        bytes.extend_from_slice(&chunk)
                    }
                    Ok(Some(_)) => return failure("HTTP JSON response exceeds 16 MiB"),
                    Ok(None) => break,
                    Err(_) => return failure("HTTP JSON response failed"),
                }
            }
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(data) => OpResult::Json(json!({ "status": status, "data": data })),
                Err(_) => failure("invalid HTTP JSON response"),
            }
        };
        match tokio::time::timeout(
            Duration::from_millis(timeout_ms).saturating_sub(started.elapsed()),
            request,
        )
        .await
        {
            Ok(result) => result,
            Err(_) => OpResult::error("ERR_PLY_HTTP_TIMEOUT", "HTTP JSON deadline exceeded"),
        }
    }

    async fn headers(&self, exchange: &Exchange) -> OpResult {
        let Some(request) = exchange.request.lock().await.take() else {
            return failure("HTTP request already sent");
        };
        let client = match self.client() {
            Ok(client) => client,
            Err(error) => return failure(error),
        };
        let mut signal = exchange.cancel.subscribe();
        let response = tokio::select! {
            biased;
            _ = cancelled(&mut signal) => return failure("HTTP request aborted"),
            response = client.execute(request) => match response {
                Ok(response) => response,
                // Do not include URLs, headers or bodies in transport errors.
                Err(error) => return failure(error.without_url().to_string()),
            },
        };
        let headers: Vec<_> = response
            .headers()
            .iter()
            .map(|(name, value)| json!([name.as_str(), String::from_utf8_lossy(value.as_bytes())]))
            .collect();
        let metadata = json!({
            "status": response.status().as_u16(),
            "statusText": response.status().canonical_reason().unwrap_or(""),
            "url": response.url().as_str(), "headers": headers,
        });
        *exchange.response.lock().await = Some(response);
        OpResult::Json(metadata)
    }

    async fn read(&self, exchange: &Exchange) -> OpResult {
        let mut response = exchange.response.lock().await;
        let Some(response) = response.as_mut() else {
            return failure("HTTP headers have not been read");
        };
        let mut signal = exchange.cancel.subscribe();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancelled(&mut signal) => return failure("HTTP request aborted"),
                chunk = response.chunk() => match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => return failure(error.without_url().to_string()),
                },
            };
            let Some(chunk) = chunk else {
                return OpResult::Json(Value::Null);
            };
            let total = exchange
                .received
                .fetch_add(chunk.len() as u64, Ordering::Relaxed)
                + chunk.len() as u64;
            if total > MAX_BODY as u64 {
                return failure("HTTP response exceeds 16 MiB");
            }
            if !chunk.is_empty() {
                return OpResult::Bytes(chunk.to_vec());
            }
        }
    }
}

/// The key `ply:http` injects for jev's JSON API, read on the host. Model
/// providers read theirs from `process.env` (RFC 0013); jev follows when it
/// moves to `fetch`.
pub(crate) fn credentials(args: &Value) -> OpResult {
    let provider = args.get("provider").and_then(Value::as_str).unwrap_or("");
    let expected = match provider {
        "jev" => "TYPESAFE_API_KEY",
        _ => return failure("provider has no host-injected key"),
    };
    if let Some(configured) = args.get("apiKeyEnv").and_then(Value::as_str)
        && configured != expected
    {
        return failure(format!("{provider} reads {expected}"));
    }
    match std::env::var(expected) {
        Ok(key) if !key.trim().is_empty() => OpResult::Json(json!(key)),
        _ => failure(format!(
            "API key environment variable is not set: {expected}"
        )),
    }
}

pub(crate) fn auth_configured(args: &Value) -> OpResult {
    OpResult::Json(json!(matches!(
        credentials(args),
        OpResult::Json(Value::String(_))
    )))
}
