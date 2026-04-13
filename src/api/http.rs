use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, Instant};
use tracing::warn;

const CONNECT_TIMEOUT_SECS: u64 = 5;
const REQUEST_TIMEOUT_SECS: u64 = 20;
const HEALTHCHECK_TIMEOUT_SECS: u64 = 10;
const MAX_ATTEMPTS: usize = 6;
const BASE_BACKOFF_MS: u64 = 250;
const RATE_LIMIT_BASE_BACKOFF_SECS: u64 = 2;

/// TMDB: 40 requests per 10 seconds = 250ms minimum gap.
/// At 90% capacity → 275ms between requests.
const RATE_LIMIT_TMDB_GAP_MS: u64 = 275;
/// TVDB: No strict published limit; ~50 req/10s is safe = 200ms gap.
/// At 90% capacity → 220ms between requests.
const RATE_LIMIT_TVDB_GAP_MS: u64 = 220;
/// Real-Debrid: ~1 request per 250ms on most endpoints.
/// During massive continuous cache syncs, 250ms triggers 429s.
/// 400ms provides a safer baseline for sustained bursts.
const RATE_LIMIT_RD_GAP_MS: u64 = 400;

// Debrid Media Manager (DMM) is a community API and can be strict.
// Real anime search-missing runs were still hitting repeated 429s at 1000ms.
// 2000ms is gentler and tends to outperform repeated backoff/retry churn.
const RATE_LIMIT_DMM_GAP_MS: u64 = 2000;

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
                let limit = TMDB_API.get_or_init(|| RateLimiter::new(RATE_LIMIT_TMDB_GAP_MS));
                limit.acquire().await;
            } else if host.contains("api4.thetvdb.com") {
                let limit = TVDB_API.get_or_init(|| RateLimiter::new(RATE_LIMIT_TVDB_GAP_MS));
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
        .expect("failed to build configured HTTP client")
}

pub fn stable_idempotency_key(namespace: &str, value: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in namespace
        .as_bytes()
        .iter()
        .chain([b':'].iter())
        .chain(value.as_bytes().iter())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }

    format!("symlinkarr-{}-{:016x}-{}", namespace, hash, value.len())
}

pub async fn send_with_retry(request: RequestBuilder) -> Result<Response> {
    // Some request bodies (e.g. multipart streams) cannot be cloned safely.
    // In those cases, run a single attempt with timeout policy only.
    if request.try_clone().is_none() {
        apply_rate_limit(&request).await;
        return Ok(request.send().await?);
    }

    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 1..=MAX_ATTEMPTS {
        let req = request
            .try_clone()
            .ok_or_else(|| anyhow::anyhow!("Failed to clone HTTP request for retry"))?;

        // Pace requests to rate-limited APIs (TMDB/TVDB) BEFORE every attempt
        apply_rate_limit(&req).await;

        let host = req
            .try_clone()
            .and_then(|r| r.build().ok())
            .and_then(|r| r.url().host_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown_host".to_string());

        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if is_retryable_status(status) && attempt < MAX_ATTEMPTS {
                    let wait = jittered_wait(retry_wait(status, resp.headers(), attempt));
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
                    let wait = jittered_wait(backoff(attempt));
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
    let req = client
        .get(&url)
        .timeout(Duration::from_secs(HEALTHCHECK_TIMEOUT_SECS))
        .header("X-Api-Key", api_key);
    let resp = send_with_retry(req).await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("{} error {}: {}", service_name, status, body);
    }
    serde_json::from_str::<serde_json::Value>(&body).map_err(|err| {
        anyhow::anyhow!(
            "{} returned non-JSON system status payload despite HTTP {}: {} ({})",
            service_name,
            status,
            body,
            err
        )
    })?;
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
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = DateTime::parse_from_rfc2822(value).ok()?;
    let retry_at = retry_at.with_timezone(&Utc);
    let wait = retry_at.signed_duration_since(Utc::now());
    Some(Duration::from_secs(wait.num_seconds().max(0) as u64))
}

fn jittered_wait(wait: Duration) -> Duration {
    let jitter_cap_ms = wait.as_millis().min(250) as u64;
    if jitter_cap_ms == 0 {
        return wait;
    }

    wait + Duration::from_millis(best_effort_jitter_ms(jitter_cap_ms + 1))
}

fn best_effort_jitter_ms(max_exclusive: u64) -> u64 {
    if max_exclusive <= 1 {
        return 0;
    }

    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_ok() {
        return u64::from_le_bytes(bytes) % max_exclusive;
    }

    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64
        % max_exclusive
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_retryable_status_429_and_5xx() {
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retryable_status(StatusCode::GATEWAY_TIMEOUT));
        assert!(!is_retryable_status(StatusCode::NOT_FOUND));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn backoff_doubles_each_attempt() {
        // BASE_BACKOFF_MS = 250
        assert_eq!(backoff(1), Duration::from_millis(250)); // 250 * 2^0
        assert_eq!(backoff(2), Duration::from_millis(500)); // 250 * 2^1
        assert_eq!(backoff(3), Duration::from_millis(1000)); // 250 * 2^2
        assert_eq!(backoff(10), Duration::from_millis(128000)); // 250 * 2^9
    }

    #[test]
    fn backoff_works_for_normal_attempts() {
        // Normal attempt range
        for attempt in 1..=20 {
            let result = backoff(attempt);
            assert!(
                result > Duration::from_secs(0),
                "attempt {} should give positive duration",
                attempt
            );
        }
    }

    #[test]
    fn retry_after_wait_parses_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "120".parse().unwrap());
        assert_eq!(retry_after_wait(&headers), Some(Duration::from_secs(120)));
    }

    #[test]
    fn retry_after_wait_returns_none_for_missing_header() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after_wait(&headers), None);
    }

    #[test]
    fn retry_after_wait_parses_http_date() {
        let mut headers = reqwest::header::HeaderMap::new();
        let retry_at: reqwest::header::HeaderValue = (Utc::now() + chrono::Duration::seconds(90))
            .to_rfc2822()
            .parse()
            .unwrap();
        headers.insert(reqwest::header::RETRY_AFTER, retry_at);

        let wait = retry_after_wait(&headers).unwrap();
        assert!(wait >= Duration::from_secs(89));
        assert!(wait <= Duration::from_secs(90));
    }

    #[test]
    fn stable_idempotency_key_is_deterministic() {
        let first = stable_idempotency_key("rd-add-magnet", "magnet:?xt=urn:btih:abc");
        let second = stable_idempotency_key("rd-add-magnet", "magnet:?xt=urn:btih:abc");
        let different = stable_idempotency_key("rd-add-magnet", "magnet:?xt=urn:btih:def");

        assert_eq!(first, second);
        assert_ne!(first, different);
    }
}
