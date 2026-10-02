mod dns;
mod proxy;
use mintcat_module_feature_proxy::{cert, config, protocol};

use anyhow::{Context, Result};
use clap::Parser;
use protocol::{ServiceIdentity, ServiceReply, ServiceRequest, SERVICE_PROTOCOL};
use std::{net::IpAddr, path::PathBuf, sync::Arc};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Parser)]
#[command(name = "mintcat-proxy", about = "Signed daemon-controlled acceleration service")]
struct Cli {
    #[arg(long, required_unless_present = "verify_protocol")]
    data_dir: Option<PathBuf>,
    #[arg(long, default_value = "8443")]
    port: u16,
    #[arg(long, default_value = "127.0.0.1")]
    bind: IpAddr,
    #[arg(long)]
    offline: bool,
    #[arg(long)]
    verify_protocol: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let identity = ServiceIdentity {
        protocol: SERVICE_PROTOCOL.into(),
        module_digest: std::env::var("MINTCAT_PROXY_DIGEST").context("daemon module digest required")?,
        module_version: std::env::var("MINTCAT_PROXY_VERSION").context("daemon module version required")?,
        instance_id: std::env::var("MINTCAT_PROXY_INSTANCE").context("daemon service identity required")?,
    };
    if cli.verify_protocol {
        // Read-only executable/protocol health: no data access, CA, network, listener or hosts.
        let mut output = tokio::io::stdout();
        let mut bytes = serde_json::to_vec(&identity)?;
        bytes.push(b'\n');
        output.write_all(&bytes).await?;
        output.flush().await?;
        return Ok(());
    }
    let data_dir = cli.data_dir.context("proxy data directory required")?;
    rustls::crypto::ring::default_provider().install_default().map_err(|_| anyhow::anyhow!("rustls provider already installed"))?;
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_ansi(false).init();
    anyhow::ensure!(data_dir.join("ca.pem").is_file() && data_dir.join("ca.key").is_file(), "CA credentials must be created through independently approved broker operation");
    let cert_mgr = Arc::new(cert::CertManager::new_or_load(&data_dir)?);
    let groups = if cli.offline {
        config::WattApiClient::load_cache(&data_dir)?
    } else {
        match config::WattApiClient::new().fetch_all().await {
            Ok(groups) => { config::WattApiClient::save_cache(&data_dir, &groups)?; groups }
            Err(error) => {
                tracing::warn!("rules fetch failed: {error}; loading cache");
                config::WattApiClient::load_cache(&data_dir).context("rules fetch failed and no cache available")?
            }
        }
    };
    let mut rules = config::flatten_projects(&groups);
    rules.extend(config::load_custom_rules(&data_dir));
    anyhow::ensure!(!rules.is_empty(), "no acceleration rules available");
    let configs = Arc::new(rules);
    let tls = rustls::ServerConfig::builder().with_no_client_auth().with_cert_resolver(cert_mgr);
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let client = reqwest::Client::builder().no_proxy()
        // Preserve the existing acceleration implementation's raw-IP upstream behavior.
        // Its certificate validation policy is documented; this is not ArtifactStore trust.
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(30))
        .dns_resolver(Arc::new(dns::ReqwestDohResolver(Arc::new(dns::DohResolver::new()))))
        .build()?;
    let listener = tokio::net::TcpListener::bind((cli.bind, cli.port)).await.context("proxy listen failed")?;
    let address = listener.local_addr()?;
    let mut output = tokio::io::stdout();
    reply(&mut output, &identity, "ready", address).await?;
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            command = input.next_line() => {
                let Some(line) = command? else { break }; // daemon loss closes control ownership
                anyhow::ensure!(line.len() <= 4096, "oversized service request");
                let request: ServiceRequest = serde_json::from_str(&line)?;
                match request {
                    ServiceRequest::Health { identity: requested } => {
                        anyhow::ensure!(requested == identity, "health identity/protocol mismatch");
                        reply(&mut output, &identity, "healthy", address).await?;
                    }
                    ServiceRequest::Stop { identity: requested } => {
                        anyhow::ensure!(requested == identity, "stop identity/protocol mismatch");
                        connections.abort_all();
                        while connections.join_next().await.is_some() {}
                        drop(listener);
                        reply(&mut output, &identity, "stopped", address).await?;
                        return Ok(());
                    }
                }
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let acceptor = acceptor.clone();
                let configs = configs.clone();
                let client = client.clone();
                connections.spawn(async move {
                    let tls = match tokio::time::timeout(std::time::Duration::from_secs(10), acceptor.accept(stream)).await {
                        Ok(Ok(tls)) => tls,
                        other => { tracing::debug!("TLS handshake from {peer}: {other:?}"); return; }
                    };
                    let service = hyper::service::service_fn(move |request| {
                        let configs = configs.clone();
                        let client = client.clone();
                        async move { proxy::handle_request(request, configs, client).await }
                    });
                    if let Err(error) = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                        .serve_connection(hyper_util::rt::TokioIo::new(tls), service).await {
                        tracing::debug!("proxy connection: {error}");
                    }
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}

async fn reply(output: &mut tokio::io::Stdout, identity: &ServiceIdentity, state: &str, address: std::net::SocketAddr) -> Result<()> {
    let mut bytes = serde_json::to_vec(&ServiceReply { identity: identity.clone(), state: state.into(), address })?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await?;
    Ok(())
}
