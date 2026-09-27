#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use da_adapter_celestia::CelestiaNetwork;
use da_adapter_mock::{MockConfig, MockNetwork};
use da_light_core::{ConfidenceReport, DANetwork};
use da_light_node::api::router;
use da_light_node::{Node, NodeConfig, StatusSnapshot};
use tokio::net::TcpListener;

#[derive(Debug, Parser)]
#[command(
    name = "sampling-node",
    version,
    about = "Modular data availability sampling light node"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate a local DA header, sample it, and serve the HTTP API
    Start(StartArgs),
    /// Show node status from a running sampling-node
    Status(ClientArgs),
    /// Ask a running node to sample the latest header again
    Sample(ClientArgs),
    /// Print the availability confidence of a running node
    Confidence(ClientArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum NetworkKind {
    Mock,
    Celestia,
}

#[derive(Debug, Args)]
struct StartArgs {
    /// Address the HTTP API binds to
    #[arg(long, default_value = "127.0.0.1:8080")]
    listen: SocketAddr,
    /// New samples to draw on each round
    #[arg(long, default_value_t = 16)]
    samples: u32,
    /// Shares in each mock data square
    #[arg(long, default_value_t = 256)]
    shares: u32,
    /// How many mock headers to generate
    #[arg(long, default_value_t = 1)]
    headers: u32,
    /// Highest-index mock shares that will be refused
    #[arg(long, default_value_t = 0)]
    withhold: u32,
    /// In-flight sample requests
    #[arg(long, default_value_t = 8)]
    concurrency: usize,
    /// Times to request a share the peer does not return
    #[arg(long, default_value_t = 3)]
    sample_attempts: u32,
    /// Missing-share fraction that makes a block unreconstructable
    #[arg(long, default_value_t = 0.25)]
    unavailable_fraction: f64,
    /// Which DA adapter to sample
    #[arg(long, value_enum, default_value_t = NetworkKind::Mock)]
    network: NetworkKind,
    /// Celestia node JSON-RPC URL, used when --network celestia
    #[arg(long, default_value = "http://127.0.0.1:26658")]
    celestia_rpc: String,
    /// Bearer token for the Celestia node RPC, when the node requires auth
    #[arg(long)]
    celestia_token: Option<String>,
    /// SQLite file that keeps sampling progress across restarts
    #[arg(long, default_value = "sampling-node.db")]
    data: PathBuf,
}

impl std::fmt::Display for NetworkKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mock => f.write_str("mock"),
            Self::Celestia => f.write_str("celestia"),
        }
    }
}

#[derive(Debug, Args)]
struct ClientArgs {
    /// Base URL of a running sampling-node
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    url: String,
    /// Print the raw JSON response
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match Cli::parse().command {
        Command::Start(args) => start(args).await,
        Command::Status(args) => status(args).await,
        Command::Sample(args) => sample(args).await,
        Command::Confidence(args) => confidence(args).await,
    }
}

async fn start(args: StartArgs) -> anyhow::Result<()> {
    let config = NodeConfig {
        listen_addr: args.listen,
        samples_per_header: args.samples,
        concurrency: args.concurrency,
        sample_attempts: args.sample_attempts,
        unavailable_fraction: args.unavailable_fraction,
        upstream_id: match args.network {
            NetworkKind::Mock => "mock-local".into(),
            NetworkKind::Celestia => "celestia".into(),
        },
        upstream_endpoint: match args.network {
            NetworkKind::Mock => "local".into(),
            NetworkKind::Celestia => args.celestia_rpc.clone(),
        },
        data_path: Some(args.data),
    };

    let network: Arc<dyn DANetwork> = match args.network {
        NetworkKind::Mock => {
            let mock = MockNetwork::generate(MockConfig {
                share_count: args.shares,
                header_count: args.headers,
                withheld_per_header: args.withhold,
                share_size: 64,
            })?;
            Arc::new(mock)
        }
        NetworkKind::Celestia => {
            let mut network = CelestiaNetwork::new(&args.celestia_rpc);
            if let Some(token) = &args.celestia_token {
                network = network.with_token(token);
            }
            Arc::new(network)
        }
    };

    let node = Arc::new(Node::new(network, config)?);
    let report = node.sample_latest().await?;
    print_report(&report, false)?;

    let app = router(Arc::clone(&node));
    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("failed to bind {}", args.listen))?;
    let addr = listener.local_addr()?;
    println!("listening on http://{addr}");
    println!("from another terminal: sampling-node status --url http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_err() {
        tracing::error!("failed to listen for ctrl-c");
    }
    tracing::info!("shutting down");
}

async fn status(args: ClientArgs) -> anyhow::Result<()> {
    let snapshot: StatusSnapshot = request_json(&args.url, reqwest::Method::GET, "/status").await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    println!("listen address: {}", snapshot.listen_addr);
    println!("samples per header: {}", snapshot.samples_per_header);
    println!("unavailable fraction: {}", snapshot.unavailable_fraction);
    println!("headers tracked: {}", snapshot.headers_tracked);
    println!("peers:");
    for peer in &snapshot.peers {
        println!("  {} {} score {}", peer.id, peer.endpoint, peer.score);
    }
    if let Some(latest) = &snapshot.latest {
        println!(
            "latest height {}: confidence {:.2}% ({})",
            latest.height,
            latest.confidence * 100.0,
            latest.level
        );
    }
    Ok(())
}

async fn sample(args: ClientArgs) -> anyhow::Result<()> {
    let report: ConfidenceReport =
        request_json(&args.url, reqwest::Method::POST, "/sample").await?;
    print_report(&report, args.json)
}

async fn confidence(args: ClientArgs) -> anyhow::Result<()> {
    let report: ConfidenceReport =
        request_json(&args.url, reqwest::Method::GET, "/confidence").await?;
    print_report(&report, args.json)
}

async fn request_json<T: serde::de::DeserializeOwned>(
    url: &str,
    method: reqwest::Method,
    path: &str,
) -> anyhow::Result<T> {
    let base = url.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let response = client
        .request(method, format!("{base}{path}"))
        .send()
        .await
        .with_context(|| {
            format!("request to {base}{path} failed; is sampling-node start running?")
        })?;
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes);
        anyhow::bail!("{status}: {body}");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn print_report(report: &ConfidenceReport, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!("header {} (height {})", report.header_id, report.height);
    println!(
        "confidence {:.2}% ({})",
        report.confidence * 100.0,
        report.level
    );
    println!(
        "successful {}  failed {}  coverage {:.4}%  new this round {}",
        report.successful_samples,
        report.failed_samples,
        report.coverage * 100.0,
        report.newly_sampled
    );
    for failure in report.last_failures.iter().take(8) {
        println!(
            "  row {} col {}: {}",
            failure.row, failure.col, failure.reason
        );
    }
    let remaining = report.last_failures.len().saturating_sub(8);
    if remaining > 0 {
        println!("  {remaining} more failures");
    }
    Ok(())
}
