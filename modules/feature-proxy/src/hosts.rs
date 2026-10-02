use anyhow::{Context, Result};
use std::path::PathBuf;

const MARKER_START: &str = "# mintcat-proxy Start";
const MARKER_END: &str = "# mintcat-proxy End";

pub fn hosts_path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\Windows\System32\drivers\etc\hosts")
    } else {
        PathBuf::from("/etc/hosts")
    }
}

pub fn update_hosts(domains: &[(String, String)]) -> Result<()> {
    for (domain, ip) in domains {
        anyhow::ensure!(!domain.is_empty() && domain.len() <= 253 && domain.split('.').all(|label| !label.is_empty() && label.len() <= 63 && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')), "invalid exact hosts domain");
        let _: std::net::IpAddr = ip.parse().context("invalid hosts address")?;
    }
    if domains.is_empty() {
        return Ok(());
    }

    let path = hosts_path();
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let cleaned = remove_marker_section(&content);

    let mut new_section = String::new();
    new_section.push_str(MARKER_START);
    new_section.push('\n');
    for (domain, ip) in domains {
        new_section.push_str(&format!("{} {}\n", ip, domain));
    }
    new_section.push_str(MARKER_END);
    new_section.push('\n');

    let final_content = format!("{}\n{}", cleaned.trim_end(), new_section);

    std::fs::write(&path, final_content).with_context(|| {
        format!(
            "failed to write {}. Are you running with sudo/admin?",
            path.display()
        )
    })?;

    tracing::info!("updated hosts file with {} entries", domains.len());
    Ok(())
}

pub fn cleanup_hosts() -> Result<()> {
    let path = hosts_path();
    let content = std::fs::read_to_string(&path).context("cannot read system hosts for cleanup")?;

    if !content.contains(MARKER_START) {
        return Ok(());
    }

    let cleaned = remove_marker_section(&content);
    std::fs::write(&path, cleaned.trim_end().to_string() + "\n").with_context(|| {
        format!(
            "failed to write {}. Are you running with sudo/admin?",
            path.display()
        )
    })?;

    tracing::info!("cleaned up hosts file");
    Ok(())
}

fn remove_marker_section(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut inside_marker = false;

    for line in content.lines() {
        if line.trim() == MARKER_START {
            inside_marker = true;
            continue;
        }
        if line.trim() == MARKER_END {
            inside_marker = false;
            continue;
        }
        if !inside_marker {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}
