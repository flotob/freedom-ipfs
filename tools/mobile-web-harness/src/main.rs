use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, RANGE};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

const DEFAULT_CORPUS: &str = "tools/mobile-web-harness/corpus/mobile-web.json";

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Black-box mobile web readiness harness for freedom-ipfs gateways"
)]
struct Args {
    /// Existing gateway URL to test, for example http://127.0.0.1:50017.
    #[arg(long, env = "GATEWAY_URL")]
    gateway_url: Option<String>,
    /// Standalone gateway binary to spawn when --gateway-url is not provided.
    #[arg(long, env = "FREEDOM_IPFS_GATEWAY_BIN")]
    gateway_bin: Option<PathBuf>,
    /// JSON corpus file.
    #[arg(long, default_value = DEFAULT_CORPUS)]
    corpus: PathBuf,
    /// Optional JSON report output path.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Request timeout in seconds.
    #[arg(long, default_value_t = 180)]
    timeout_secs: u64,
    /// Gateway request concurrency budget when spawning a gateway.
    #[arg(long, default_value_t = 4)]
    max_concurrent_requests: usize,
    /// Gateway routing mode when spawning a gateway.
    #[arg(long, default_value = "auto")]
    routing_mode: String,
    /// DHT query timeout when spawning a gateway.
    #[arg(long, default_value_t = 10)]
    dht_query_timeout_secs: u64,
    /// Max DHT providers when spawning a gateway.
    #[arg(long, default_value_t = 4)]
    dht_max_providers: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let corpus = Corpus::read(&args.corpus)?;
    let mut spawned = None;
    let gateway_url = if let Some(url) = args.gateway_url.as_deref() {
        normalize_gateway_url(url)
    } else {
        let gateway = SpawnedGateway::start(&args).await?;
        let url = gateway.url.clone();
        spawned = Some(gateway);
        url
    };

    let report = run_corpus(
        &gateway_url,
        &corpus,
        Duration::from_secs(args.timeout_secs),
    )
    .await;
    if let Some(mut gateway) = spawned {
        gateway.stop().await;
    }

    let report = report?;
    print_summary(&report);
    if let Some(output) = args.output {
        let json = serde_json::to_string_pretty(&report)?;
        std::fs::write(&output, json).with_context(|| format!("write {}", output.display()))?;
        eprintln!("wrote report to {}", output.display());
    }

    if report.results.iter().any(|result| !result.passed) {
        bail!("mobile web harness found failures");
    }
    Ok(())
}

async fn run_corpus(gateway_url: &str, corpus: &Corpus, timeout: Duration) -> Result<RunReport> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .context("build reqwest client")?;
    let mut results = Vec::new();
    for entry in &corpus.entries {
        results.push(run_case(&client, gateway_url, entry).await);
    }
    Ok(RunReport {
        gateway_url: gateway_url.to_string(),
        generated_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        results,
    })
}

async fn run_case(client: &reqwest::Client, gateway_url: &str, entry: &CorpusEntry) -> CaseResult {
    let url = format!("{}{}", gateway_url.trim_end_matches('/'), entry.path);
    let method = entry.method.as_deref().unwrap_or("GET");
    let started = Instant::now();
    let response = match method {
        "GET" => {
            let mut request = client.get(&url);
            if let Some(range) = &entry.range {
                request = request.header(RANGE, range);
            }
            request.send().await
        }
        "HEAD" => client.head(&url).send().await,
        other => {
            return CaseResult::failed(
                entry,
                url,
                vec![format!(
                    "unsupported method {other}; only GET and HEAD are supported"
                )],
            );
        }
    };
    let response = match response {
        Ok(response) => response,
        Err(err) => {
            return CaseResult::failed(entry, url, vec![format!("request error: {err}")]);
        }
    };

    let ttfb_ms = started.elapsed().as_millis();
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let content_range = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = match response.bytes().await {
        Ok(body) => body,
        Err(err) => {
            return CaseResult::failed(entry, url, vec![format!("body error: {err}")]);
        }
    };
    let total_ms = started.elapsed().as_millis();
    let body_preview = String::from_utf8_lossy(&body.iter().copied().take(180).collect::<Vec<_>>())
        .replace('\n', "\\n");
    let mut failures = Vec::new();

    if let Some(expected) = entry.expect_status {
        if status != expected {
            failures.push(format!("status {status}, expected {expected}"));
        }
    }
    if let Some(expected) = &entry.expect_content_type_prefix {
        if !content_type
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .starts_with(&expected.to_ascii_lowercase())
        {
            failures.push(format!(
                "content-type {:?}, expected prefix {expected:?}",
                content_type
            ));
        }
    }
    if let Some(expected) = &entry.expect_content_range_prefix {
        if !content_range.as_deref().unwrap_or("").starts_with(expected) {
            failures.push(format!(
                "content-range {:?}, expected prefix {expected:?}",
                content_range
            ));
        }
    }
    if let Some(expected) = &entry.expect_body_contains {
        let text = String::from_utf8_lossy(&body);
        if !text.contains(expected) {
            failures.push(format!("body did not contain {expected:?}"));
        }
    }
    if let Some(min_bytes) = entry.min_bytes {
        if body.len() < min_bytes {
            failures.push(format!(
                "body {} bytes, expected at least {min_bytes}",
                body.len()
            ));
        }
    }
    if let Some(max_ttfb_ms) = entry.max_ttfb_ms {
        if ttfb_ms > max_ttfb_ms as u128 {
            failures.push(format!("TTFB {ttfb_ms}ms exceeded {max_ttfb_ms}ms"));
        }
    }

    CaseResult {
        id: entry.id.clone(),
        description: entry.description.clone(),
        method: method.to_string(),
        url,
        status: Some(status),
        content_type,
        content_range,
        body_bytes: body.len(),
        ttfb_ms,
        total_ms,
        body_preview,
        passed: failures.is_empty(),
        failures,
    }
}

fn print_summary(report: &RunReport) {
    println!("gateway: {}", report.gateway_url);
    for result in &report.results {
        let mark = if result.passed { "PASS" } else { "FAIL" };
        println!(
            "{mark} {:32} status={} type={} bytes={} ttfb={}ms total={}ms",
            result.id,
            result
                .status
                .map(|status| status.to_string())
                .unwrap_or_else(|| "-".to_string()),
            result.content_type.as_deref().unwrap_or("-"),
            result.body_bytes,
            result.ttfb_ms,
            result.total_ms
        );
        for failure in &result.failures {
            println!("  - {failure}");
        }
    }
}

fn normalize_gateway_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

#[derive(Debug)]
struct SpawnedGateway {
    child: Child,
    url: String,
    stderr_task: Option<JoinHandle<()>>,
}

impl SpawnedGateway {
    async fn start(args: &Args) -> Result<Self> {
        let bin = args
            .gateway_bin
            .clone()
            .unwrap_or_else(|| PathBuf::from("target/debug/freedom-ipfs-gateway"));
        let mut command = Command::new(&bin);
        command
            .kill_on_drop(true)
            .arg("--online")
            .arg("--routing-mode")
            .arg(&args.routing_mode)
            .arg("--max-concurrent-requests")
            .arg(args.max_concurrent_requests.to_string())
            .arg("--dht-query-timeout-secs")
            .arg(args.dht_query_timeout_secs.to_string())
            .arg("--dht-max-providers")
            .arg(args.dht_max_providers.to_string())
            .arg("--addr")
            .arg("127.0.0.1:0")
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().with_context(|| {
            format!(
                "spawn {}; build it first with `cargo build -p freedom-ipfs-gateway`",
                bin.display()
            )
        })?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("gateway stderr was not piped"))?;
        let mut lines = BufReader::new(stderr).lines();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let line = tokio::time::timeout_at(deadline, lines.next_line())
                .await
                .context("timed out waiting for gateway listen address")?
                .context("read gateway stderr")?;
            let Some(line) = line else {
                bail!("gateway exited before reporting listen address");
            };
            if let Some(url) = line.strip_prefix("gateway listening on ") {
                let stderr_task = tokio::spawn(async move {
                    while let Ok(Some(line)) = lines.next_line().await {
                        eprintln!("gateway: {line}");
                    }
                });
                return Ok(Self {
                    child,
                    url: normalize_gateway_url(url),
                    stderr_task: Some(stderr_task),
                });
            }
            eprintln!("gateway: {line}");
        }
    }

    async fn stop(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        if let Some(stderr_task) = self.stderr_task.take() {
            stderr_task.abort();
            let _ = stderr_task.await;
        }
    }
}

#[derive(Debug, Deserialize)]
struct Corpus {
    entries: Vec<CorpusEntry>,
}

impl Corpus {
    fn read(path: &PathBuf) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
    }
}

#[derive(Debug, Deserialize)]
struct CorpusEntry {
    id: String,
    description: Option<String>,
    path: String,
    method: Option<String>,
    range: Option<String>,
    expect_status: Option<u16>,
    expect_content_type_prefix: Option<String>,
    expect_content_range_prefix: Option<String>,
    expect_body_contains: Option<String>,
    min_bytes: Option<usize>,
    max_ttfb_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
struct RunReport {
    gateway_url: String,
    generated_at_unix_seconds: u64,
    results: Vec<CaseResult>,
}

#[derive(Debug, Serialize)]
struct CaseResult {
    id: String,
    description: Option<String>,
    method: String,
    url: String,
    status: Option<u16>,
    content_type: Option<String>,
    content_range: Option<String>,
    body_bytes: usize,
    ttfb_ms: u128,
    total_ms: u128,
    body_preview: String,
    passed: bool,
    failures: Vec<String>,
}

impl CaseResult {
    fn failed(entry: &CorpusEntry, url: String, failures: Vec<String>) -> Self {
        Self {
            id: entry.id.clone(),
            description: entry.description.clone(),
            method: entry.method.clone().unwrap_or_else(|| "GET".to_string()),
            url,
            status: None,
            content_type: None,
            content_range: None,
            body_bytes: 0,
            ttfb_ms: 0,
            total_ms: 0,
            body_preview: String::new(),
            passed: false,
            failures,
        }
    }
}
