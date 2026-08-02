use std::fs;
use std::net::{IpAddr, UdpSocket};
use std::path::Path;

use axum_server::tls_rustls::RustlsConfig;
use rcgen::{generate_simple_self_signed, CertifiedKey};

/// Best-effort detection of this machine's LAN-facing IP: connect a UDP
/// socket (no packets actually sent - `connect()` on UDP just picks a route)
/// and read back which local interface the OS would use, without requiring
/// real connectivity to the target address.
fn detect_lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip())
}

fn desired_sans() -> Vec<String> {
    let mut sans = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if let Some(ip) = detect_lan_ip() {
        let ip_string = ip.to_string();
        if !sans.contains(&ip_string) {
            sans.push(ip_string);
        }
    }
    sans
}

fn sans_marker_matches(marker_path: &Path, sans: &[String]) -> bool {
    let Ok(existing) = fs::read_to_string(marker_path) else {
        return false;
    };
    existing.lines().collect::<Vec<_>>() == sans.iter().map(String::as_str).collect::<Vec<_>>()
}

/// Loads a cached self-signed cert/key pair from `cert_dir` if one already
/// exists covering the current SAN list, so a routine restart doesn't
/// retrigger a fresh "untrusted certificate" browser warning - generating
/// and caching a new pair otherwise (first run, or the detected LAN IP
/// changed since the cached cert was made).
///
/// Returns the loaded TLS config plus the SAN list it covers, so the caller
/// can print out exactly which URL(s) are valid to visit.
pub async fn ensure_tls_config(
    cert_dir: &str,
) -> Result<(RustlsConfig, Vec<String>), Box<dyn std::error::Error>> {
    let dir = Path::new(cert_dir);
    fs::create_dir_all(dir)?;
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    let marker_path = dir.join("sans.txt");

    let sans = desired_sans();
    let needs_generation =
        !cert_path.exists() || !key_path.exists() || !sans_marker_matches(&marker_path, &sans);

    if needs_generation {
        tracing::info!(?sans, "generating self-signed TLS certificate");
        let CertifiedKey { cert, signing_key } = generate_simple_self_signed(sans.clone())?;
        fs::write(&cert_path, cert.pem())?;
        fs::write(&key_path, signing_key.serialize_pem())?;
        fs::write(&marker_path, sans.join("\n"))?;
    } else {
        tracing::info!(?sans, "reusing cached self-signed TLS certificate");
    }

    let config = RustlsConfig::from_pem_file(&cert_path, &key_path).await?;
    Ok((config, sans))
}
