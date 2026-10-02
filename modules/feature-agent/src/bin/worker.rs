#[tokio::main]
async fn main() {
    if mintcat_feature_agent::run_native_owner().await.is_err() {
        // Protocol payloads and provider diagnostics never reach stderr.
        eprintln!("Agent owner protocol failed");
        std::process::exit(1);
    }
}
