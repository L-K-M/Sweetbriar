//! The pinned-certificate verifier (spec/10). The pin is the identity:
//! the server name is not checked, but the SHA-256 must equal the pin
//! and the handshake signatures and validity are still verified. Setup
//! also reads pins: from Bridge's exported `cert.pem`, or by observing
//! the presented leaf in a handshake this side always refuses.

use std::fmt;
use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
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

    /// The pin from its 64 hex digits (either case), as
    /// `trusted_cert_sha256` stores it; anything else is no pin.
    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let mut pin = [0u8; 32];
        let (pairs, _) = bytes.as_chunks::<2>();
        for (byte, [high, low]) in pin.iter_mut().zip(pairs) {
            let digit = |ascii: u8| {
                char::from(ascii)
                    .to_digit(16)
                    .and_then(|v| u8::try_from(v).ok())
            };
            *byte = digit(*high)? << 4 | digit(*low)?;
        }
        Some(Self(pin))
    }

    /// The raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The pin in lowercase hex, for diagnostics.
    pub fn hex(&self) -> String {
        hex(&self.0)
    }

    /// The pin of the one certificate in Bridge's exported `cert.pem`
    /// (spec/10's preferred path). A file holding a private key (Bridge's
    /// `key.pem`, or a bundle with one) is refused from its `BEGIN` line
    /// alone, before anything in it is decoded: Sweetbriar never reads
    /// the key. Otherwise exactly one certificate must decode and parse
    /// as X.509, so a wrong file fails here rather than as a pin that
    /// never matches.
    pub fn from_pem(pem: &[u8]) -> Result<Self, PemError> {
        if holds_private_key(pem) {
            return Err(PemError::PrivateKey);
        }
        let mut certificates = CertificateDer::pem_slice_iter(pem);
        let certificate = match certificates.next() {
            None => return Err(PemError::NoCertificate),
            Some(Err(_)) => return Err(PemError::Malformed),
            Some(Ok(certificate)) => certificate,
        };
        match certificates.next() {
            None => {}
            Some(Ok(_)) => return Err(PemError::SeveralCertificates),
            // One certificate, then a section that does not decode.
            Some(Err(_)) => return Err(PemError::Malformed),
        }
        ParsedCertificate::try_from(&certificate).map_err(|_| PemError::Malformed)?;
        Ok(Self::of_der(certificate.as_ref()))
    }
}

/// Whether any line opens a private-key section (`PRIVATE KEY`,
/// `RSA PRIVATE KEY`, `ENCRYPTED PRIVATE KEY`, ...). Only the
/// boundary lines are inspected; no section body is touched.
fn holds_private_key(pem: &[u8]) -> bool {
    const BEGIN: &[u8] = b"-----BEGIN ";
    const PRIVATE_KEY: &[u8] = b"PRIVATE KEY";
    pem.split(|byte| *byte == b'\n' || *byte == b'\r')
        .map(<[u8]>::trim_ascii)
        .filter_map(|line| line.strip_prefix(BEGIN))
        .any(|label| {
            label
                .windows(PRIVATE_KEY.len())
                .any(|window| window == PRIVATE_KEY)
        })
}

/// Why a picked file yields no pin. No variant carries file content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PemError {
    /// A private-key section: `key.pem`, or a bundle holding a key.
    /// Refused unread.
    PrivateKey,
    /// No `CERTIFICATE` section: not a certificate file.
    NoCertificate,
    /// More than one certificate: Bridge exports exactly one leaf.
    SeveralCertificates,
    /// A section that does not decode, or a certificate that does not
    /// parse as X.509 — including one certificate followed by damage.
    Malformed,
}

impl fmt::Display for PemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PrivateKey => {
                "the file holds a private key; choose the cert.pem Bridge exported, not key.pem"
            }
            Self::NoCertificate => {
                "the file holds no certificate; choose the cert.pem Bridge exported"
            }
            Self::SeveralCertificates => "the file holds more than one certificate",
            Self::Malformed => "the certificate file is damaged or not a certificate",
        })
    }
}

impl std::error::Error for PemError {}

impl fmt::Debug for CertificatePin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CertificatePin({})", self.hex())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// What the verifier does with the presented leaf.
#[derive(Clone, Copy, Debug)]
enum Trust {
    /// Accept only the pinned leaf, whose validity is still checked.
    Pinned(CertificatePin),
    /// Setup's fingerprint fallback (spec/10): record the presented
    /// leaf and refuse, so no session is ever established.
    Observe,
}

/// The verifier plus the cell it records the presented leaf's pin into
/// on a refusal, so the handshake failure can carry it (spec/10).
struct CertPinVerifier {
    trust: Trust,
    algs: WebPkiSupportedAlgorithms,
    presented: Arc<Mutex<Option<CertificatePin>>>,
}

impl fmt::Debug for CertPinVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertPinVerifier")
            .field("trust", &self.trust)
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
        let presented = CertificatePin::of_der(end_entity.as_ref());
        let pinned = match self.trust {
            Trust::Pinned(pin) => pin == presented,
            Trust::Observe => false,
        };
        if !pinned {
            *self
                .presented
                .lock()
                .expect("a panicking verifier held the mismatch cell") = Some(presented);
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

/// A client config accepting only `pin`, and the cell a mismatch
/// records the presented leaf's pin into.
pub(crate) fn pinned_client_config(
    pin: CertificatePin,
) -> (Arc<ClientConfig>, Arc<Mutex<Option<CertificatePin>>>) {
    client_config(Trust::Pinned(pin))
}

/// A client config that records the presented leaf's pin and refuses
/// every handshake (spec/10's fingerprint fallback).
pub(crate) fn observing_client_config() -> (Arc<ClientConfig>, Arc<Mutex<Option<CertificatePin>>>) {
    client_config(Trust::Observe)
}

fn client_config(trust: Trust) -> (Arc<ClientConfig>, Arc<Mutex<Option<CertificatePin>>>) {
    let presented = Arc::new(Mutex::new(None));
    let verifier = CertPinVerifier {
        trust,
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
    fn a_pin_round_trips_through_its_hex() {
        let pin = CertificatePin::of_der(sb_test_support::cert_der().as_ref());
        assert_eq!(CertificatePin::from_hex(&pin.hex()), Some(pin));
        assert_eq!(
            CertificatePin::from_hex(&pin.hex().to_uppercase()),
            Some(pin)
        );
        for bad in [
            "",
            "ab",
            &"0".repeat(63),
            &"g".repeat(64),
            &"00:".repeat(21),
        ] {
            assert_eq!(CertificatePin::from_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn pin_is_the_sha256_of_exact_der_bytes() {
        let cert = sb_test_support::cert_der();
        let pin = CertificatePin::of_der(cert.as_ref());
        assert_eq!(
            pin.hex(),
            "f921c0432dab5a70401266a0216f46e6f6f795de47868447e9fde159771a24d1"
        );
        assert_eq!(CertificatePin::from_bytes(*pin.as_bytes()), pin);
    }

    #[test]
    fn exact_pin_accepts_a_valid_certificate_but_a_wrong_pin_reports_the_presented_hash() {
        let cert = sb_test_support::cert_der();
        let mut verifier = CertPinVerifier {
            trust: Trust::Pinned(CertificatePin::of_der(cert.as_ref())),
            algs: rustls::crypto::ring::default_provider().signature_verification_algorithms,
            presented: Arc::new(Mutex::new(None)),
        };
        let name = ServerName::try_from("127.0.0.1").unwrap();
        assert!(
            verifier
                .verify_server_cert(&cert, &[], &name, &[], UnixTime::now())
                .is_ok()
        );
        assert!(verifier.presented.lock().unwrap().is_none());
        verifier.trust = Trust::Pinned(CertificatePin::from_bytes([0; 32]));
        assert!(matches!(
            verifier.verify_server_cert(&cert, &[], &name, &[], UnixTime::now()),
            Err(rustls::Error::InvalidCertificate(
                CertificateError::UnknownIssuer
            ))
        ));
        assert_eq!(
            verifier.presented.lock().unwrap().map(|pin| pin.hex()),
            Some("f921c0432dab5a70401266a0216f46e6f6f795de47868447e9fde159771a24d1".to_owned())
        );
    }

    #[test]
    fn the_observing_verifier_records_the_leaf_and_refuses() {
        let cert = sb_test_support::cert_der();
        let verifier = CertPinVerifier {
            trust: Trust::Observe,
            algs: rustls::crypto::ring::default_provider().signature_verification_algorithms,
            presented: Arc::new(Mutex::new(None)),
        };
        let name = ServerName::try_from("127.0.0.1").unwrap();
        assert!(matches!(
            verifier.verify_server_cert(&cert, &[], &name, &[], UnixTime::now()),
            Err(rustls::Error::InvalidCertificate(
                CertificateError::UnknownIssuer
            ))
        ));
        assert_eq!(
            *verifier.presented.lock().unwrap(),
            Some(CertificatePin::of_der(cert.as_ref()))
        );
    }

    #[test]
    fn cert_pem_yields_the_pin_of_its_one_certificate() {
        let pin = CertificatePin::from_pem(sb_test_support::CERT_PEM.as_bytes()).unwrap();
        assert_eq!(
            pin,
            CertificatePin::of_der(sb_test_support::cert_der().as_ref())
        );
    }

    #[test]
    fn files_that_are_not_one_certificate_yield_no_pin() {
        let cert = sb_test_support::CERT_PEM;
        let key = sb_test_support::KEY_PEM;
        // key.pem, and any bundle holding a key, is refused unread.
        assert_eq!(
            CertificatePin::from_pem(key.as_bytes()),
            Err(PemError::PrivateKey)
        );
        assert_eq!(
            CertificatePin::from_pem(format!("{cert}{key}").as_bytes()),
            Err(PemError::PrivateKey)
        );
        for label in ["RSA PRIVATE KEY", "EC PRIVATE KEY", "ENCRYPTED PRIVATE KEY"] {
            let other = format!("{cert}-----BEGIN {label}-----\nAAAA\n-----END {label}-----\n");
            assert_eq!(
                CertificatePin::from_pem(other.as_bytes()),
                Err(PemError::PrivateKey),
                "{label}"
            );
        }
        assert_eq!(CertificatePin::from_pem(b""), Err(PemError::NoCertificate));
        assert_eq!(
            CertificatePin::from_pem(b"just some text\n"),
            Err(PemError::NoCertificate)
        );
        // One certificate, then a section that does not decode: damage,
        // not a second certificate.
        assert_eq!(
            CertificatePin::from_pem(
                format!("{cert}-----BEGIN CERTIFICATE-----\n!!\n-----END CERTIFICATE-----\n")
                    .as_bytes()
            ),
            Err(PemError::Malformed)
        );
        assert_eq!(
            CertificatePin::from_pem(
                format!("{cert}-----BEGIN CERTIFICATE-----\nAAAA\n").as_bytes()
            ),
            Err(PemError::Malformed)
        );
        assert_eq!(
            CertificatePin::from_pem(format!("{cert}{cert}").as_bytes()),
            Err(PemError::SeveralCertificates)
        );
        assert_eq!(
            CertificatePin::from_pem(
                b"-----BEGIN CERTIFICATE-----\n!!not base64!!\n-----END CERTIFICATE-----\n"
            ),
            Err(PemError::Malformed)
        );
        // Valid base64, but the bytes are not an X.509 certificate.
        assert_eq!(
            CertificatePin::from_pem(
                b"-----BEGIN CERTIFICATE-----\nAAECAwQFBgcICQ==\n-----END CERTIFICATE-----\n"
            ),
            Err(PemError::Malformed)
        );
    }

    #[test]
    fn matching_pin_does_not_bypass_certificate_validity() {
        let cert = sb_test_support::cert_der();
        let verifier = CertPinVerifier {
            trust: Trust::Pinned(CertificatePin::of_der(cert.as_ref())),
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
