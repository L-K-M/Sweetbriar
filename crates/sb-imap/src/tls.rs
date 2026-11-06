//! The pinned-certificate verifier (spec/10). The pin is the identity:
//! the server name is not checked, but the SHA-256 must equal the pin
//! and the handshake signatures and validity are still verified.

use std::fmt;
use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use sha2::{Digest, Sha256};

/// A 32-byte SHA-256 pin of the leaf certificate's DER (spec/10).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CertificatePin([u8; 32]);

impl CertificatePin {
    /// The pin of a DER-encoded certificate.
    pub fn of_der(der: &[u8]) -> Self {
        let digest = Sha256::digest(der);
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&digest);
        Self(bytes)
    }

    /// The pin from its raw 32 bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The pin in lowercase hex, for diagnostics.
    pub fn hex(&self) -> String {
        hex(&self.0)
    }
}

impl fmt::Debug for CertificatePin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CertificatePin({})", self.hex())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The verifier plus the cell a pin mismatch records the presented
/// hash into, so the handshake failure can carry it (spec/10).
struct CertPinVerifier {
    pin: CertificatePin,
    algs: WebPkiSupportedAlgorithms,
    presented: Arc<Mutex<Option<String>>>,
}

impl fmt::Debug for CertPinVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertPinVerifier")
            .field("pin", &self.pin)
            .finish()
    }
}

impl ServerCertVerifier for CertPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let digest = Sha256::digest(end_entity.as_ref());
        if digest.as_slice() != self.pin.as_bytes() {
            *self
                .presented
                .lock()
                .expect("a panicking verifier held the mismatch cell") = Some(hex(&digest));
            return Err(CertificateError::UnknownIssuer.into());
        }

        // The pin matched; validity still applies, checked as WebPKI
        // does with the leaf as its own anchor. The server name is not
        // checked: the pin is the identity (spec/10).
        let mut roots = RootCertStore::empty();
        roots.add(end_entity.clone()).map_err(|err| {
            rustls::Error::General(format!(
                "the pinned certificate is not a trust anchor: {err}"
            ))
        })?;
        let parsed = ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_cert_signed_by_trust_anchor(
            &parsed,
            &roots,
            intermediates,
            now,
            self.algs.all,
        )?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

pub(crate) fn pinned_client_config(
    pin: CertificatePin,
) -> (Arc<ClientConfig>, Arc<Mutex<Option<String>>>) {
    let presented = Arc::new(Mutex::new(None));
    let verifier = CertPinVerifier {
        pin,
        algs: rustls::crypto::ring::default_provider().signature_verification_algorithms,
        presented: Arc::clone(&presented),
    };
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    (Arc::new(config), presented)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn matching_pin_does_not_bypass_certificate_validity() {
        let cert = sb_test_support::cert_der();
        let verifier = CertPinVerifier {
            pin: CertificatePin::of_der(cert.as_ref()),
            algs: rustls::crypto::ring::default_provider().signature_verification_algorithms,
            presented: Arc::new(Mutex::new(None)),
        };
        let result = verifier.verify_server_cert(
            &cert,
            &[],
            &ServerName::try_from("127.0.0.1").expect("server name"),
            &[],
            UnixTime::since_unix_epoch(Duration::ZERO),
        );
        assert!(matches!(
            result,
            Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidYetContext { .. }
            )) | Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidYet
            ))
        ));
    }
}
