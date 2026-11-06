//! TLS identity and trust.
//!
//! Every machine has a self-signed certificate generated on first start.
//! TLS verifiers accept any certificate during the handshake (still checking
//! the handshake signatures, so the peer must hold the private key); trust is
//! decided afterwards by comparing the certificate's SHA-256 fingerprint with
//! the list of paired machines. Pairing derives a 6-digit code from both
//! fingerprints, so a man in the middle would produce a different code.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};


pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: Vec<u8>,
    pub fingerprint: String,
}

impl Identity {
    pub fn load_or_create(dir: &std::path::Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let cert_path = dir.join("identity.der");
        let key_path = dir.join("identity.key");
        let (cert, key) = if cert_path.exists() && key_path.exists() {
            (std::fs::read(&cert_path)?, std::fs::read(&key_path)?)
        } else {
            let ck = rcgen::generate_simple_self_signed(vec!["shared-mouse-keyboard".to_string()])?;
            let cert = ck.cert.der().to_vec();
            let key = ck.signing_key.serialize_der();
            std::fs::write(&cert_path, &cert)?;
            write_private(&key_path, &key)?;
            (cert, key)
        };
        let fingerprint = fingerprint(&cert);
        Ok(Self {
            cert: CertificateDer::from(cert),
            key,
            fingerprint,
        })
    }

    fn key_der(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()))
    }

    pub fn server_config(&self) -> anyhow::Result<rustls::ServerConfig> {
        let cfg = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?
            .with_client_cert_verifier(Arc::new(AcceptAnyCert))
            .with_single_cert(vec![self.cert.clone()], self.key_der())?;
        Ok(cfg)
    }

    pub fn client_config(&self) -> anyhow::Result<rustls::ClientConfig> {
        let cfg = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
            .with_client_auth_cert(vec![self.cert.clone()], self.key_der())?;
        Ok(cfg)
    }
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
}

pub fn fingerprint(cert_der: &[u8]) -> String {
    hex::encode(Sha256::digest(cert_der))
}

/// Short code both sides can compute from the two fingerprints.
pub fn pairing_code(fp_a: &str, fp_b: &str) -> String {
    let (lo, hi) = if fp_a <= fp_b { (fp_a, fp_b) } else { (fp_b, fp_a) };
    let digest = Sha256::digest(format!("smk-pair-v1|{lo}|{hi}").as_bytes());
    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{n:06}")
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

#[derive(Debug)]
struct AcceptAnyCert;

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &provider().signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &provider().signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider().signature_verification_algorithms.supported_schemes()
    }
}

impl ClientCertVerifier for AcceptAnyCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &provider().signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &provider().signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider().signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_symmetric() {
        assert_eq!(pairing_code("aa", "bb"), pairing_code("bb", "aa"));
        assert_ne!(pairing_code("aa", "bb"), pairing_code("aa", "cc"));
        assert_eq!(pairing_code("aa", "bb").len(), 6);
    }
}
