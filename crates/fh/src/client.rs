use config::{TlsConfig, TlsSetting};
use protocol::{create_client_config, MaybeTlsStream};
use std::path::Path;
use tokio::net::TcpStream;

/// Resolves TLS configuration by merging `.farhand.yaml` configuration with CLI overrides.
pub fn resolve_tls_config(
    config_tls: Option<&TlsSetting>,
    cli_tls: bool,
    cli_ca: Option<&Path>,
    cli_fingerprint: Option<&str>,
    cli_insecure: bool,
    cli_cert: Option<&Path>,
    cli_key: Option<&Path>,
) -> Option<TlsConfig> {
    let mut resolved = config_tls.map(|s| s.to_config()).unwrap_or_default();

    if cli_tls {
        resolved.enabled = true;
    }
    if let Some(ca) = cli_ca {
        resolved.ca = Some(ca.to_string_lossy().to_string());
        resolved.enabled = true;
    }
    if let Some(fp) = cli_fingerprint {
        resolved.fingerprint = Some(fp.to_string());
        resolved.enabled = true;
    }
    if cli_insecure {
        resolved.insecure = true;
        resolved.enabled = true;
    }
    if let Some(cert) = cli_cert {
        resolved.cert = Some(cert.to_string_lossy().to_string());
        resolved.enabled = true;
    }
    if let Some(key) = cli_key {
        resolved.key = Some(key.to_string_lossy().to_string());
        resolved.enabled = true;
    }

    if resolved.enabled {
        Some(resolved)
    } else {
        None
    }
}

/// Connects to a remote agent daemon over raw TCP or TLS depending on configuration.
pub async fn connect_to_agent(
    host: &str,
    tls_config: Option<&TlsConfig>,
) -> Result<MaybeTlsStream<TcpStream>, Box<dyn std::error::Error + Send + Sync>> {
    let tcp_stream = TcpStream::connect(host).await?;
    if let Some(tls) = tls_config {
        if tls.enabled {
            let server_host = host.split(':').next().unwrap_or(host);
            let ca_pem = if let Some(ref ca_path) = tls.ca {
                Some(std::fs::read_to_string(ca_path)?)
            } else {
                None
            };
            let client_cert = match (&tls.cert, &tls.key) {
                (Some(c), Some(k)) => {
                    let c_str = std::fs::read_to_string(c)?;
                    let k_str = std::fs::read_to_string(k)?;
                    Some((c_str, k_str))
                }
                _ => None,
            };

            let client_cert_ref = client_cert.as_ref().map(|(c, k)| (c.as_str(), k.as_str()));
            let client_config = create_client_config(
                ca_pem.as_deref(),
                tls.fingerprint.as_deref(),
                tls.insecure,
                client_cert_ref,
            )?;

            let tls_stream = protocol::connect_tls(tcp_stream, server_host, client_config).await?;
            return Ok(MaybeTlsStream::Client(tls_stream));
        }
    }
    Ok(MaybeTlsStream::Plain(tcp_stream))
}
