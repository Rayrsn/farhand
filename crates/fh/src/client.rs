use config::{TlsConfig, TlsSetting};
use protocol::{create_client_config, MaybeTlsStream};
use std::path::Path;
use tokio::net::TcpStream;

/// Extract the server hostname from a `host:port` listen string, handling
/// bracketed IPv6 literals (`[::1]:9876`) that a naive `split(':')` would
/// mangle into `"["` (breaking TLS SNI).
pub fn split_server_host(listen: &str) -> &str {
    if let Some(start) = listen.find('[') {
        if let Some(rel_end) = listen[start..].find(']') {
            return &listen[start + 1..start + rel_end];
        }
    }
    if listen.matches(':').count() == 1 {
        // IPv4 or DNS name with a port.
        return listen.split(':').next().unwrap_or(listen);
    }
    // Bare IPv6 literal without a port.
    listen
}

/// Parse a reverse-port-forward specification `LOCAL:REMOTE`.
/// Malformed specs are a hard error, never silently ignored.
pub fn parse_forward_spec(spec: &str) -> Result<(u16, u16), String> {
    let (local, remote) = spec
        .split_once(':')
        .ok_or_else(|| "expected LOCAL:REMOTE (e.g. 3000:3000)".to_string())?;
    let local_port = local
        .trim()
        .parse::<u16>()
        .map_err(|_| format!("invalid local port '{}'", local))?;
    let remote_port = remote
        .trim()
        .parse::<u16>()
        .map_err(|_| format!("invalid remote port '{}'", remote))?;
    if local_port == 0 || remote_port == 0 {
        return Err("ports must be non-zero".to_string());
    }
    Ok((local_port, remote_port))
}

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
            let server_host = split_server_host(host);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_server_host_handles_ipv4_dns_and_ipv6() {
        assert_eq!(split_server_host("127.0.0.1:9876"), "127.0.0.1");
        assert_eq!(split_server_host("example.com:9876"), "example.com");
        assert_eq!(split_server_host("[::1]:9876"), "::1");
        assert_eq!(split_server_host("[2001:db8::1]:9876"), "2001:db8::1");
        // Bare IPv6 without a port: nothing to strip.
        assert_eq!(split_server_host("::1"), "::1");
    }

    #[test]
    fn parse_forward_spec_validates_strictly() {
        assert_eq!(parse_forward_spec("3000:3000").unwrap(), (3000, 3000));
        assert!(parse_forward_spec("3000").is_err(), "missing separator");
        assert!(parse_forward_spec("a:b").is_err(), "non-numeric ports");
        assert!(parse_forward_spec("0:3000").is_err(), "zero local port");
        assert!(parse_forward_spec("70000:3000").is_err(), "out of range");
    }
}
