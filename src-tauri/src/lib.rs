pub mod agent;
pub mod app;
pub mod control_plane;
pub mod download;
pub mod frontend;
pub mod hot_update;
pub mod integrator;
pub mod network;
pub mod webview_host;

pub fn run() {
    app::run()
}
