//! A small client for the InstaBox REST API (https://instabox.sh/docs/api).
//!
//! - Auth: `X-API-Key`, read from the profile's `api_key_env` when a call is made.
//! - Changes answer `202` with an operation; [`Api::wait_op`] follows it to the end.
//! - Every change carries an `Idempotency-Key`, reused across retries of the same request.
//! - Errors branch on `error.code`; a `retryable` error is retried after `retryAfterSeconds`
//!   (capped), a few times.

use anyhow::{bail, Context, Result};
use rand::distributions::Alphanumeric;
use rand::Rng;
use reqwest::Method;
use serde_json::Value;
use std::time::{Duration, Instant};

/// An API error with its machine-readable code.
#[derive(Debug, thiserror::Error)]
#[error("instabox {status} {code}: {message}")]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub retry_after: Option<u64>,
}

pub fn api_code(e: &anyhow::Error) -> Option<&str> {
    e.downcast_ref::<ApiError>().map(|a| a.code.as_str())
}

#[derive(Debug, Clone)]
pub struct Api {
    http: reqwest::Client,
    pub base: String,
    pub key_env: String,
    /// Fixed key (tests); else read from `key_env` per call.
    pub key: Option<String>,
    /// Interval between operation polls.
    pub poll: Duration,
    /// How long an operation may take before we give up.
    pub op_timeout: Duration,
    /// Retries of a `retryable` error.
    pub retries: u32,
    /// Longest wait between retries.
    pub retry_cap: Duration,
}

/// What to send with a request besides the body.
#[derive(Default)]
pub struct Opts<'a> {
    /// Send an `Idempotency-Key` (every lifecycle change).
    pub idempotent: bool,
    pub headers: &'a [(&'a str, String)],
}

impl Api {
    pub fn new(base: &str, key_env: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(660))
                .build()
                .expect("reqwest client"),
            base: base.trim_end_matches('/').to_string(),
            key_env: key_env.to_string(),
            key: None,
            poll: Duration::from_secs(2),
            op_timeout: Duration::from_secs(600),
            retries: 3,
            retry_cap: Duration::from_secs(30),
        }
    }

    fn key(&self) -> Result<String> {
        if let Some(k) = &self.key {
            return Ok(k.clone());
        }
        match std::env::var(&self.key_env) {
            Ok(k) if !k.is_empty() => Ok(k),
            _ => bail!(
                "instabox: no API key; set ${} (create one at https://dashboard.instabox.sh/dashboard/api-keys)",
                self.key_env
            ),
        }
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.call(Method::GET, path, None, Opts::default()).await
    }

    pub async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        opts: Opts<'_>,
    ) -> Result<Value> {
        let key = self.key()?;
        // Every change carries a key ([LIVE]: `/connections` refuses a POST without one);
        // `exec` is the one documented exception.
        let wants_key = opts.idempotent || (method != Method::GET && !path.ends_with("/exec"));
        let idem: Option<String> = wants_key.then(|| {
            rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(24)
                .map(char::from)
                .collect()
        });
        let url = format!("{}{path}", self.base);
        let mut attempt = 0;
        loop {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("X-API-Key", &key)
                .header("Accept", "application/json");
            if let Some(k) = &idem {
                req = req.header("Idempotency-Key", k);
            }
            for (h, v) in opts.headers {
                req = req.header(*h, v);
            }
            if let Some(b) = &body {
                req = req.json(b);
            }
            let resp = req
                .send()
                .await
                .with_context(|| format!("instabox {method} {path}"))?;
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            if (200..300).contains(&status) {
                if text.trim().is_empty() {
                    return Ok(Value::Null);
                }
                return serde_json::from_str(&text)
                    .with_context(|| format!("instabox {method} {path}: bad JSON"));
            }
            let err = parse_error(status, &text);
            if err.retryable && attempt < self.retries {
                attempt += 1;
                let wait = Duration::from_secs(err.retry_after.unwrap_or(2)).min(self.retry_cap);
                tracing::debug!(
                    "instabox {} retryable, retry {attempt} in {wait:?}",
                    err.code
                );
                tokio::time::sleep(wait).await;
                continue;
            }
            return Err(err.into());
        }
    }

    /// Follow an operation (`{id, state, ...}`) until it ends. `Ok` with the final operation
    /// on `succeeded`; `Err` with its error on `failed` or after `op_timeout`.
    pub async fn wait_op(&self, op: &Value) -> Result<Value> {
        // An operation always names its machine; anything else (a machine, `null`) is an
        // endpoint that answered with its result directly.
        let (Some(id), true) = (op["id"].as_str(), op.get("machineId").is_some()) else {
            return Ok(op.clone());
        };
        let started = Instant::now();
        let mut cur = op.clone();
        loop {
            match cur["state"].as_str() {
                Some("succeeded") => return Ok(cur),
                Some("failed") => bail!(
                    "instabox {} {} failed: {}",
                    cur["type"].as_str().unwrap_or("operation"),
                    id,
                    cur["error"]["message"]
                        .as_str()
                        .or_else(|| cur["error"]["code"].as_str())
                        .unwrap_or("no reason given")
                ),
                _ => {}
            }
            if started.elapsed() > self.op_timeout {
                bail!(
                    "instabox operation {id} did not finish in {:?}",
                    self.op_timeout
                );
            }
            tokio::time::sleep(self.poll).await;
            cur = self.get(&format!("/v1/operations/{id}")).await?;
        }
    }
}

fn parse_error(status: u16, text: &str) -> ApiError {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let e = &v["error"];
    ApiError {
        status,
        code: e["code"].as_str().unwrap_or("http_error").to_string(),
        message: e["message"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| text.chars().take(200).collect()),
        retryable: e["retryable"].as_bool().unwrap_or(false),
        retry_after: e["retryAfterSeconds"].as_u64(),
    }
}
