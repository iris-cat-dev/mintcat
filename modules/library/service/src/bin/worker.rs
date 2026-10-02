#[tokio::main]
async fn main() {
    if mintcat_library_service::run_native_owner().await.is_err() {
        // Never print credential-bearing protocol or database payloads.
        eprintln!("Library owner protocol failed");
        std::process::exit(1);
    }
}
