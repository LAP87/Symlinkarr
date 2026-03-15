use anyhow::Result;
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, Instant};
use tracing::warn;

const CONNECT_TIMEOUT_SECS: u64 = 5;
const REQUEST_TIMEOUT_SECS: u64 = 20;
const MAX_ATTEMPTS: usize = 6;
const BASE_BACKOFF_MS: u64 = 250;
const RATE_LIMIT_BASE_BACKOFF_SECS: u64 = 2;

/// Minimum gap between requests to rate-limited APIs.
/// TMDB allows ~40 requests per 10 seconds. We use 280ms to provide
/// padding against network jitter and edge-server race conditions.
const RATE_LIMIT_MIN_GAP_MS: u64 = 280;
// Real-Debrid allows ~1 request per 250ms on most endpoints.
// During massive continuous cache syncs, 250ms is too aggressive and triggers 429s.
// 400ms provides a safer conservative baseline.
const RATE_LIMIT_RD_GAP_MS: u64 = 400;

// Debrid Media Manager (DMM) is a community API and can be strict.
// 1000ms provides a gentle pace during auto-acquire search bursts.
const RATE_LIMIT_DMM_GAP_MS: u64 = 1000;

struct RateLimiter {
    sender: mpsc::Sender<oneshot::Sender<()>>,
}

impl RateLimiter {
    fn new(gap_ms: u64) -> Self {
        let (tx, mut rx) = mpsc::channel::<oneshot::Sender<()>>(1000);
        
        tokio::spawn(async move {
            let gap = Duration::from_millis(gap_ms);
            let mut last = Instant::now() - gap; // Allow first request immediately
            
            while let Some(req_tx) = rx.recv().await {
                let now = Instant::now();
                let elapsed = now.saturating_duration_since(last);
                
                if elapsed < gap {
                    sleep(gap - elapsed).await;
                    last = Instant::now();
                } else {
                    last = now;
                }
                
                // Signal the requester that it's their turn
                let _ = req_tx.send(());
            }
        });
        
        Self { sender: tx }
    }
    
    async fn acquire(&self) {
        let (tx, rx) = oneshot::channel();
        if self.sender.send(tx).await.is_ok() {
            let _ = rx.await;
        }
    }
}

static TMDB_API: OnceLock<RateLimiter> = OnceLock::new();
static TVDB_API: OnceLock<RateLimiter> = OnceLock::new();
static RD_API: OnceLock<RateLimiter> = OnceLock::new();
static DMM_API: OnceLock<RateLimiter> = OnceLock::new();

pub async fn apply_rate_limit(req: &RequestBuilder) {
    if let Some(req_clone) = req.try_clone() {
        if let Ok(built_req) = req_clone.build() {
            let url = built_req.url();
            let host = url.host_str().unwrap_or("");
            
            if host.contains("api.themoviedb.org") {
                let limit = TMDB_API.get_or_init(|| RateLimiter::new(RATE_LIMIT_MIN_GAP_MS));
                limit.acquire().await;
            } else if host.contains("api4.thetvdb.com") {
                let limit = TVDB_API.get_or_init(|| RateLimiter::new(RATE_LIMIT_MIN_GAP_MS));
                limit.acquire().await;
            } else if host.contains("api.real-debrid.com") {
                let limit = RD_API.get_or_init(|| RateLimiter::new(RATE_LIMIT_RD_GAP_MS));
                limit.acquire().await;
            } else if host.contains("debridmediamanager.com") {
                let limit = DMM_API.get_or_init(|| RateLimiter::new(RATE_LIMIT_DMM_GAP_MS));
                limit.acquire().await;
            }
        }
    }
}

pub fn build_client() -> Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(32)
        .tcp_keepalive(Duration::from_secs(60))
        .build()
        .unwrap_or_else(|_| Client::new())
}

pub async fn send_with_retry(request: RequestBuilder) -> Result<Response> {
    // Some request bodies (e.g. multipart streams) cannot be cloned safely.
    // In those cases, run a single attempt with timeout policy only.
    if request.try_clone().is_none() {
        return Ok(request.send().await?);
    }

    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 1..=MAX_ATTEMPTS {
        let req = request
            .try_clone()
            .ok_or_else(|| anyhow::anyhow!("Failed to clone HTTP request for retry"))?;

        // Pace requests to rate-limited APIs (TMDB/TVDB) BEFORE every attempt
        apply_rate_limit(&req).await;

        let host = req.try_clone().and_then(|r| r.build().ok()).and_then(|r| r.url().host_str().map(|s| s.to_string())).unwrap_or_else(|| "unknown_host".to_string());

        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if is_retryable_status(status) && attempt < MAX_ATTEMPTS {
                    let wait = retry_wait(status, resp.headers(), attempt);
                    warn!(
                        "HTTP retry on status {} for {} (attempt {}/{}), waiting {:?}",
                        status, host, attempt, MAX_ATTEMPTS, wait
                    );
                    sleep(wait).await;
                    continue;
                }
                return Ok(resp);
            }
            Err(err) => {
                if is_retryable_error(&err) && attempt < MAX_ATTEMPTS {
                    let wait = backoff(attempt);
                    warn!(
                        "HTTP retry on transport error '{}' (attempt {}/{}), waiting {:?}",
                        err, attempt, MAX_ATTEMPTS, wait
                    );
                    sleep(wait).await;
                    continue;
                }
                last_err = Some(err.into());
                break;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("HTTP request failed after retries")))
}

/// Performs a GET health-check against an *arr-style `/api/{version}/system/status` endpoint.
/// Returns `Ok(())` on success, bails with a descriptive error on failure.
pub async fn check_system_status(
    client: &Client,
    base_url: &str,
    api_key: &str,
    api_version: &str,
    service_name: &str,
) -> anyhow::Result<()> {
    let url = format!("{}/api/{}/system/status", base_url, api_version);
    let req = client.get(&url).header("X-Api-Key", api_key);
    let resp = send_with_retry(req).await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("{} error {}: {}", service_name, status, body);
    }
    Ok(())
}

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn is_retryable_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

fn backoff(attempt: usize) -> Duration {
    let factor = 1u64 << (attempt.saturating_sub(1) as u32);
    Duration::from_millis(BASE_BACKOFF_MS.saturating_mul(factor))
}

fn retry_wait(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    attempt: usize,
) -> Duration {
    if status == StatusCode::TOO_MANY_REQUESTS {
        if let Some(wait) = retry_after_wait(headers) {
            return wait;
        }

        let factor = 1u64 << (attempt.saturating_sub(1) as u32);
        return Duration::from_secs(RATE_LIMIT_BASE_BACKOFF_SECS.saturating_mul(factor));
    }

    backoff(attempt)
}

fn retry_after_wait(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?;
    let value = value.to_str().ok()?.trim();
    let seconds: u64 = value.parse().ok()?;
    Some(Duration::from_secs(seconds))
}
