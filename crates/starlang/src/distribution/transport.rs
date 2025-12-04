//! QUIC transport layer for distribution.
//!
//! Handles the low-level QUIC connections using the `quinn` crate.

use super::protocol::{DistError, DistMessage, frame_message, parse_frame};
use quinn::{
    ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig, TransportConfig,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// QUIC endpoint wrapper for distribution.
pub struct QuicTransport {
    /// The QUIC endpoint.
    endpoint: Endpoint,
    /// Our node name for handshakes.
    node_name: String,
    /// Our creation number.
    creation: u32,
}

impl QuicTransport {
    /// Create a new QUIC transport bound to the given address.
    ///
    /// If cert/key paths are provided, loads them. Otherwise generates self-signed.
    /// This endpoint can both accept incoming connections and make outgoing connections.
    pub async fn bind(
        addr: SocketAddr,
        node_name: String,
        creation: u32,
        cert_path: Option<&Path>,
        key_path: Option<&Path>,
    ) -> Result<Self, DistError> {
        let (server_config, _certs) = if let (Some(cert), Some(key)) = (cert_path, key_path) {
            load_certs(cert, key)?
        } else {
            generate_self_signed(&node_name)?
        };

        // Create client config for outgoing connections
        let client_config = configure_client()?;

        let mut endpoint =
            Endpoint::server(server_config, addr).map_err(|e| DistError::Io(e.to_string()))?;

        // Set client config so this endpoint can also make outgoing connections
        endpoint.set_default_client_config(client_config);

        tracing::info!(%addr, "QUIC transport listening");

        Ok(Self {
            endpoint,
            node_name,
            creation,
        })
    }

    /// Create a client-only endpoint for outgoing connections.
    pub fn client(node_name: String, creation: u32) -> Result<Self, DistError> {
        let client_config = configure_client()?;

        let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())
            .map_err(|e| DistError::Io(e.to_string()))?;
        endpoint.set_default_client_config(client_config);

        Ok(Self {
            endpoint,
            node_name,
            creation,
        })
    }

    /// Accept an incoming connection.
    pub async fn accept(&self) -> Option<QuicConnection> {
        let incoming = self.endpoint.accept().await?;
        let connection = incoming.await.ok()?;

        tracing::debug!(
            remote = %connection.remote_address(),
            "Accepted incoming connection"
        );

        Some(QuicConnection::new(connection))
    }

    /// Connect to a remote node.
    pub async fn connect(
        &self,
        addr: SocketAddr,
        server_name: &str,
    ) -> Result<QuicConnection, DistError> {
        let connection = self
            .endpoint
            .connect(addr, server_name)
            .map_err(|e| DistError::Connect(e.to_string()))?
            .await
            .map_err(|e| DistError::Connect(e.to_string()))?;

        tracing::debug!(%addr, "Connected to remote node");

        Ok(QuicConnection::new(connection))
    }

    /// Get the local address this transport is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr, DistError> {
        self.endpoint
            .local_addr()
            .map_err(|e| DistError::Io(e.to_string()))
    }

    /// Get our node name.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// Get our creation number.
    pub fn creation(&self) -> u32 {
        self.creation
    }

    /// Close the endpoint.
    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"shutdown");
    }
}

/// A QUIC connection to a remote node.
pub struct QuicConnection {
    connection: Connection,
}

impl QuicConnection {
    /// Create a new connection wrapper.
    pub fn new(connection: Connection) -> Self {
        Self { connection }
    }

    /// Open a new bidirectional stream for sending a message.
    pub async fn open_stream(&self) -> Result<(SendStream, RecvStream), DistError> {
        self.connection
            .open_bi()
            .await
            .map_err(|e| DistError::Io(e.to_string()))
    }

    /// Accept an incoming stream.
    pub async fn accept_stream(&self) -> Result<(SendStream, RecvStream), DistError> {
        self.connection
            .accept_bi()
            .await
            .map_err(|e| DistError::Io(e.to_string()))
    }

    /// Send a message on this connection.
    ///
    /// Opens a new stream, sends the framed message, and closes the send side.
    pub async fn send_message(&self, msg: &DistMessage) -> Result<(), DistError> {
        let (mut send, _recv) = self.open_stream().await?;

        let frame = frame_message(msg)?;
        send.write_all(&frame)
            .await
            .map_err(|e| DistError::Io(e.to_string()))?;
        send.finish().map_err(|e| DistError::Io(e.to_string()))?;

        Ok(())
    }

    /// Receive a message from a stream.
    pub async fn recv_message(recv: &mut RecvStream) -> Result<DistMessage, DistError> {
        let mut buf = Vec::new();

        loop {
            let mut chunk = [0u8; 4096];
            match recv.read(&mut chunk).await {
                Ok(Some(n)) => {
                    buf.extend_from_slice(&chunk[..n]);

                    // Try to parse a complete message
                    if let Some((msg, _consumed)) = parse_frame(&buf)? {
                        return Ok(msg);
                    }
                }
                Ok(None) => {
                    // Stream closed
                    if buf.is_empty() {
                        return Err(DistError::ConnectionClosed);
                    }
                    // Try to parse what we have
                    if let Some((msg, _)) = parse_frame(&buf)? {
                        return Ok(msg);
                    }
                    return Err(DistError::Decode("incomplete message".to_string()));
                }
                Err(e) => return Err(DistError::Io(e.to_string())),
            }
        }
    }

    /// Get the remote address.
    pub fn remote_address(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    /// Close this connection.
    pub fn close(&self, reason: &str) {
        self.connection.close(0u32.into(), reason.as_bytes());
    }

    /// Check if the connection is still open.
    pub fn is_open(&self) -> bool {
        self.connection.close_reason().is_none()
    }
}

/// Generate a self-signed certificate for development.
fn generate_self_signed(
    name: &str,
) -> Result<(ServerConfig, Vec<CertificateDer<'static>>), DistError> {
    let cert = rcgen::generate_simple_self_signed(vec![name.to_string()])
        .map_err(|e| DistError::Tls(e.to_string()))?;

    let cert_der = CertificateDer::from(cert.cert);
    let key_der = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());

    let mut server_config =
        ServerConfig::with_single_cert(vec![cert_der.clone()], PrivateKeyDer::Pkcs8(key_der))
            .map_err(|e| DistError::Tls(e.to_string()))?;

    // Configure transport
    let mut transport = TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(10)));
    transport.max_idle_timeout(Some(Duration::from_secs(60).try_into().unwrap()));
    server_config.transport_config(Arc::new(transport));

    Ok((server_config, vec![cert_der]))
}

/// Load certificates from files.
fn load_certs(
    cert_path: &Path,
    key_path: &Path,
) -> Result<(ServerConfig, Vec<CertificateDer<'static>>), DistError> {
    let cert_pem = std::fs::read(cert_path)
        .map_err(|e| DistError::Tls(format!("failed to read cert: {}", e)))?;
    let key_pem = std::fs::read(key_path)
        .map_err(|e| DistError::Tls(format!("failed to read key: {}", e)))?;

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .filter_map(|r| r.ok())
        .collect();

    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .map_err(|e| DistError::Tls(format!("failed to parse key: {}", e)))?
        .ok_or_else(|| DistError::Tls("no private key found".to_string()))?;

    let mut server_config = ServerConfig::with_single_cert(certs.clone(), key)
        .map_err(|e| DistError::Tls(e.to_string()))?;

    let mut transport = TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(10)));
    transport.max_idle_timeout(Some(Duration::from_secs(60).try_into().unwrap()));
    server_config.transport_config(Arc::new(transport));

    Ok((server_config, certs))
}

/// Configure the QUIC client.
fn configure_client() -> Result<ClientConfig, DistError> {
    // For development, skip certificate verification
    // In production, this should be configured properly
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    let mut client_config = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .map_err(|e| DistError::Tls(e.to_string()))?,
    ));

    let mut transport = TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(10)));
    transport.max_idle_timeout(Some(Duration::from_secs(60).try_into().unwrap()));
    client_config.transport_config(Arc::new(transport));

    Ok(client_config)
}

/// Skip server certificate verification (for development).
///
/// WARNING: This is insecure and should not be used in production.
#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}
