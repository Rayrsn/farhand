use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, DigitallySignedStruct, Error as RustlsError, RootCertStore, ServerConfig,
    SignatureScheme,
};
use sha2::{Digest, Sha256};
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
pub use tokio_rustls::client::TlsStream as ClientTlsStream;
pub use tokio_rustls::server::TlsStream as ServerTlsStream;
pub use tokio_rustls::{TlsAcceptor, TlsConnector};

#[derive(Error, Debug)]
pub enum TlsError {
    #[error("TLS configuration error: {0}")]
    Config(String),

    #[error("Certificate error: {0}")]
    Certificate(String),

    #[error("Private key error: {0}")]
    PrivateKey(String),

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Rustls error: {0}")]
    Rustls(#[from] RustlsError),
}

/// A stream that can be either plain or encrypted via TLS (client or server).
pub enum MaybeTlsStream<S> {
    Plain(S),
    Client(ClientTlsStream<S>),
    Server(ServerTlsStream<S>),
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for MaybeTlsStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            MaybeTlsStream::Client(s) => Pin::new(s).poll_read(cx, buf),
            MaybeTlsStream::Server(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for MaybeTlsStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            MaybeTlsStream::Client(s) => Pin::new(s).poll_write(cx, buf),
            MaybeTlsStream::Server(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_flush(cx),
            MaybeTlsStream::Client(s) => Pin::new(s).poll_flush(cx),
            MaybeTlsStream::Server(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            MaybeTlsStream::Client(s) => Pin::new(s).poll_shutdown(cx),
            MaybeTlsStream::Server(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Compute SHA-256 fingerprint in lowercase hex of DER-encoded certificate bytes.
pub fn compute_cert_fingerprint(cert_der: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cert_der);
    hex::encode(hasher.finalize()).to_lowercase()
}

/// Normalize fingerprint string by removing "sha256:", colons, spaces, and converting to lowercase.
pub fn normalize_fingerprint(input: &str) -> String {
    let trimmed = input.trim();
    let without_prefix = trimmed
        .strip_prefix("sha256:")
        .or_else(|| trimmed.strip_prefix("SHA256:"))
        .unwrap_or(trimmed);
    without_prefix
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_lowercase()
}

/// Self-signed certificate bundle generated in-memory.
#[derive(Debug, Clone)]
pub struct GeneratedCert {
    pub cert_pem: String,
    pub key_pem: String,
    pub cert_der: Vec<u8>,
    pub fingerprint: String,
}

/// Generate a self-signed ECDSA certificate for localhost and specified SANs.
pub fn generate_self_signed_cert(
    mut subject_alt_names: Vec<String>,
) -> Result<GeneratedCert, TlsError> {
    if !subject_alt_names.iter().any(|s| s == "localhost") {
        subject_alt_names.push("localhost".to_string());
    }
    if !subject_alt_names.iter().any(|s| s == "127.0.0.1") {
        subject_alt_names.push("127.0.0.1".to_string());
    }

    let params = rcgen::CertificateParams::new(subject_alt_names)
        .map_err(|e| TlsError::Config(format!("Failed to build cert params: {}", e)))?;

    let key_pair = rcgen::KeyPair::generate()
        .map_err(|e| TlsError::Config(format!("Failed to generate key pair: {}", e)))?;

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| TlsError::Config(format!("Failed to generate self-signed cert: {}", e)))?;

    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();
    let cert_der = cert.der().to_vec();
    let fingerprint = compute_cert_fingerprint(&cert_der);

    Ok(GeneratedCert {
        cert_pem,
        key_pem,
        cert_der,
        fingerprint,
    })
}

/// Custom verifier that skips certificate validation (for --tls-insecure).
#[derive(Debug)]
pub struct InsecureServerCertVerifier;

impl ServerCertVerifier for InsecureServerCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Custom verifier that checks the server's certificate against a pinned SHA-256 fingerprint.
#[derive(Debug)]
pub struct FingerprintServerCertVerifier {
    pub expected_fingerprint: String,
}

impl ServerCertVerifier for FingerprintServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        let actual = compute_cert_fingerprint(end_entity.as_ref());
        if actual == self.expected_fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(RustlsError::General(format!(
                "Certificate fingerprint mismatch: expected {}, got {}",
                self.expected_fingerprint, actual
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Simple helper to parse PEM certificate chain into CertificateDer items.
pub fn parse_pem_certs(pem_str: &str) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let mut certs = Vec::new();
    let mut in_cert = false;
    let mut current_base64 = String::new();

    for line in pem_str.lines() {
        let trimmed = line.trim();
        if trimmed == "-----BEGIN CERTIFICATE-----" {
            in_cert = true;
            current_base64.clear();
        } else if trimmed == "-----END CERTIFICATE-----" {
            if in_cert {
                in_cert = false;
                let der = base64_decode_simple(&current_base64)?;
                certs.push(CertificateDer::from(der));
            }
        } else if in_cert {
            current_base64.push_str(trimmed);
        }
    }

    if certs.is_empty() {
        return Err(TlsError::Certificate(
            "No valid certificates found in PEM data".into(),
        ));
    }
    Ok(certs)
}

/// Simple helper to parse PEM private key into PrivateKeyDer item.
pub fn parse_pem_private_key(pem_str: &str) -> Result<PrivateKeyDer<'static>, TlsError> {
    let mut in_pkcs8 = false;
    let mut in_pkcs1 = false;
    let mut in_sec1 = false;
    let mut current_base64 = String::new();

    for line in pem_str.lines() {
        let trimmed = line.trim();
        if trimmed == "-----BEGIN PRIVATE KEY-----" {
            in_pkcs8 = true;
            current_base64.clear();
        } else if trimmed == "-----END PRIVATE KEY-----" {
            if in_pkcs8 {
                let der = base64_decode_simple(&current_base64)?;
                return Ok(PrivateKeyDer::Pkcs8(der.into()));
            }
        } else if trimmed == "-----BEGIN RSA PRIVATE KEY-----" {
            in_pkcs1 = true;
            current_base64.clear();
        } else if trimmed == "-----END RSA PRIVATE KEY-----" {
            if in_pkcs1 {
                let der = base64_decode_simple(&current_base64)?;
                return Ok(PrivateKeyDer::Pkcs1(der.into()));
            }
        } else if trimmed == "-----BEGIN EC PRIVATE KEY-----" {
            in_sec1 = true;
            current_base64.clear();
        } else if trimmed == "-----END EC PRIVATE KEY-----" {
            if in_sec1 {
                let der = base64_decode_simple(&current_base64)?;
                return Ok(PrivateKeyDer::Sec1(der.into()));
            }
        } else if in_pkcs8 || in_pkcs1 || in_sec1 {
            current_base64.push_str(trimmed);
        }
    }

    Err(TlsError::PrivateKey(
        "No supported private key (PKCS#8, PKCS#1, SEC1) found in PEM data".into(),
    ))
}

fn base64_decode_simple(input: &str) -> Result<Vec<u8>, TlsError> {
    // Pure standard RFC4648 base64 decoding without extra dependencies
    let mut clean: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'\r' && b != b'\n' && b != b' ')
        .collect();

    // Pad if necessary
    while clean.len() % 4 != 0 {
        clean.push(b'=');
    }

    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;

    for &b in &clean {
        if b == b'=' {
            break;
        }
        let val = match b {
            b'A'..=b'Z' => (b - b'A') as u32,
            b'a'..=b'z' => (b - b'a' + 26) as u32,
            b'0'..=b'9' => (b - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(TlsError::Certificate(format!("Invalid base64 byte: {}", b))),
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }

    Ok(out)
}

/// Create a ServerConfig with the given certificate, private key, and optional client CA for mTLS.
pub fn create_server_config(
    cert_pem: &str,
    key_pem: &str,
    client_ca_pem: Option<&str>,
) -> Result<Arc<ServerConfig>, TlsError> {
    let certs = parse_pem_certs(cert_pem)?;
    let key = parse_pem_private_key(key_pem)?;

    let config = if let Some(ca_str) = client_ca_pem {
        let client_certs = parse_pem_certs(ca_str)?;
        let mut roots = RootCertStore::empty();
        for c in client_certs {
            roots.add(c).map_err(|e| {
                TlsError::Config(format!("Failed to add client CA to root store: {}", e))
            })?;
        }
        let client_verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|e| TlsError::Config(format!("Failed to build client verifier: {}", e)))?;
        ServerConfig::builder().with_client_cert_verifier(client_verifier)
    } else {
        ServerConfig::builder().with_no_client_auth()
    }
    .with_single_cert(certs, key)
    .map_err(|e| TlsError::Config(format!("Failed to configure server cert: {}", e)))?;

    Ok(Arc::new(config))
}

/// Create a ClientConfig with options for fingerprint pinning, CA validation, or skipping verification.
pub fn create_client_config(
    ca_pem: Option<&str>,
    fingerprint: Option<&str>,
    insecure: bool,
    client_cert: Option<(&str, &str)>,
) -> Result<Arc<ClientConfig>, TlsError> {
    let builder = ClientConfig::builder();

    let builder = if insecure {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(InsecureServerCertVerifier))
    } else if let Some(fp) = fingerprint {
        let norm_fp = normalize_fingerprint(fp);
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(FingerprintServerCertVerifier {
                expected_fingerprint: norm_fp,
            }))
    } else if let Some(ca_str) = ca_pem {
        let certs = parse_pem_certs(ca_str)?;
        let mut roots = RootCertStore::empty();
        for c in certs {
            roots
                .add(c)
                .map_err(|e| TlsError::Config(format!("Failed to add CA to root store: {}", e)))?;
        }
        builder.with_root_certificates(roots)
    } else {
        // Use system/mozilla webpki root certs if available, or empty roots with warning
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        builder.with_root_certificates(roots)
    };

    let config = if let Some((c_pem, k_pem)) = client_cert {
        let certs = parse_pem_certs(c_pem)?;
        let key = parse_pem_private_key(k_pem)?;
        builder
            .with_client_auth_cert(certs, key)
            .map_err(|e| TlsError::Config(format!("Failed to set client auth cert: {}", e)))?
    } else {
        builder.with_no_client_auth()
    };

    Ok(Arc::new(config))
}

/// Parse a hostname or IP string into a rustls ServerName.
pub fn parse_server_name(name: &str) -> Result<ServerName<'static>, TlsError> {
    if let Ok(ip) = name.parse::<std::net::IpAddr>() {
        return Ok(ServerName::from(ip));
    }
    ServerName::try_from(name.to_string())
        .map_err(|e| TlsError::Config(format!("Invalid server name '{}': {}", name, e)))
}

/// Connects an underlying stream (like TcpStream) over TLS using the provided client config and server name.
pub async fn connect_tls<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    server_name: &str,
    config: Arc<ClientConfig>,
) -> Result<ClientTlsStream<S>, TlsError> {
    let connector = TlsConnector::from(config);
    let domain = parse_server_name(server_name)?;
    let tls_stream = connector
        .connect(domain, stream)
        .await
        .map_err(TlsError::Io)?;
    Ok(tls_stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_self_signed_cert_and_fingerprint() {
        let cert =
            generate_self_signed_cert(vec!["localhost".to_string(), "127.0.0.1".to_string()])
                .unwrap();
        assert!(!cert.cert_pem.is_empty());
        assert!(!cert.key_pem.is_empty());
        assert_eq!(cert.fingerprint.len(), 64); // SHA-256 is 32 bytes = 64 hex characters

        let parsed_certs = parse_pem_certs(&cert.cert_pem).unwrap();
        assert_eq!(parsed_certs.len(), 1);

        let parsed_key = parse_pem_private_key(&cert.key_pem).unwrap();
        assert!(matches!(parsed_key, PrivateKeyDer::Pkcs8(_)));
    }

    #[test]
    fn test_normalize_fingerprint() {
        let raw = "SHA256:7f:9a:8b:1c:2d:3e:4f:50:61:72:83:94:a5:b6:c7:d8:e9:f0:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee";
        let norm = normalize_fingerprint(raw);
        assert_eq!(
            norm,
            "7f9a8b1c2d3e4f5061728394a5b6c7d8e9f0112233445566778899aabbccddee"
        );
    }

    #[test]
    fn test_server_and_client_config_roundtrip() {
        let cert = generate_self_signed_cert(vec!["localhost".to_string()]).unwrap();
        let server_cfg = create_server_config(&cert.cert_pem, &cert.key_pem, None).unwrap();
        assert!(server_cfg.alpn_protocols.is_empty());

        let client_cfg_fp =
            create_client_config(None, Some(&cert.fingerprint), false, None).unwrap();
        assert!(client_cfg_fp.alpn_protocols.is_empty());

        let client_cfg_insecure = create_client_config(None, None, true, None).unwrap();
        assert!(client_cfg_insecure.alpn_protocols.is_empty());
    }

    #[test]
    fn test_server_name_parsing() {
        let name1 = parse_server_name("localhost").unwrap();
        assert!(matches!(name1, ServerName::DnsName(_)));

        let name2 = parse_server_name("127.0.0.1").unwrap();
        assert!(matches!(name2, ServerName::IpAddress(_)));
    }
}
