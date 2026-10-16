//! rustls configuration with certificate pinning.
//!
//! Both sides present self-signed certificates (mTLS). The TLS layer verifies handshake
//! *signatures* (proof of key possession) but accepts any certificate; the application then
//! checks the SHA-256 fingerprint of the peer certificate against `peers.toml` (session ALPN) or
//! binds it into the SPAKE2 confirmation (pairing ALPN). No application data flows before that.

use crate::identity::{Identity, fingerprint_of};
use crate::{Fingerprint, NetError, Result};
use deskbridge_proto::{ALPN_PAIR, ALPN_SESSION};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use std::sync::Arc;

/// SNI name used for every connection; irrelevant for trust (pinning), but must be valid.
pub const SERVER_NAME: &str = "deskbridge";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

impl ClientCertVerifier for AcceptAny {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn identity_parts(id: &Identity) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    (
        vec![CertificateDer::from(id.cert_der.clone())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key_pkcs8_der.clone())),
    )
}

/// Server side: offers both ALPNs, requires a client certificate.
pub fn server_config(id: &Identity) -> Result<quinn::ServerConfig> {
    let (chain, key) = identity_parts(id);
    let prov = provider();
    let mut cfg = rustls::ServerConfig::builder_with_provider(prov.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| NetError::Tls(e.to_string()))?
        .with_client_cert_verifier(Arc::new(AcceptAny(prov)))
        .with_single_cert(chain, key)
        .map_err(|e| NetError::Tls(e.to_string()))?;
    cfg.alpn_protocols = vec![ALPN_SESSION.to_vec(), ALPN_PAIR.to_vec()];
    let quic = QuicServerConfig::try_from(cfg).map_err(|e| NetError::Tls(e.to_string()))?;
    let mut sc = quinn::ServerConfig::with_crypto(Arc::new(quic));
    sc.transport_config(Arc::new(transport_config()));
    Ok(sc)
}

/// Client side for a single ALPN.
pub fn client_config(id: &Identity, alpn: &[u8]) -> Result<quinn::ClientConfig> {
    let (chain, key) = identity_parts(id);
    let prov = provider();
    let mut cfg = rustls::ClientConfig::builder_with_provider(prov.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| NetError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(prov)))
        .with_client_auth_cert(chain, key)
        .map_err(|e| NetError::Tls(e.to_string()))?;
    cfg.alpn_protocols = vec![alpn.to_vec()];
    let quic = QuicClientConfig::try_from(cfg).map_err(|e| NetError::Tls(e.to_string()))?;
    let mut cc = quinn::ClientConfig::new(Arc::new(quic));
    cc.transport_config(Arc::new(transport_config()));
    Ok(cc)
}

/// QUIC transport tuning: keep-alive 1 s, idle timeout 10 s, generous datagram buffers.
#[must_use]
pub fn transport_config() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(std::time::Duration::from_secs(1)));
    t.max_idle_timeout(Some(
        std::time::Duration::from_secs(10)
            .try_into()
            .expect("idle timeout fits"),
    ));
    t.datagram_receive_buffer_size(Some(4 * 1024 * 1024));
    t.datagram_send_buffer_size(4 * 1024 * 1024);
    t.max_concurrent_uni_streams(256u32.into());
    t.max_concurrent_bidi_streams(16u32.into());
    t.receive_window((64u32 * 1024 * 1024).into());
    t.stream_receive_window((32u32 * 1024 * 1024).into());
    t.send_window(64 * 1024 * 1024);
    t
}

/// Fingerprint of the certificate the remote side presented during the handshake.
pub fn peer_fingerprint(conn: &quinn::Connection) -> Result<Fingerprint> {
    let ident = conn.peer_identity().ok_or(NetError::FingerprintMismatch)?;
    let certs = ident
        .downcast::<Vec<CertificateDer<'static>>>()
        .map_err(|_| NetError::Tls("unexpected peer identity type".into()))?;
    let first = certs.first().ok_or(NetError::FingerprintMismatch)?;
    Ok(fingerprint_of(first.as_ref()))
}

/// ALPN the connection negotiated.
#[must_use]
pub fn negotiated_alpn(conn: &quinn::Connection) -> Option<Vec<u8>> {
    conn.handshake_data()?
        .downcast::<quinn::crypto::rustls::HandshakeData>()
        .ok()?
        .protocol
        .clone()
}
