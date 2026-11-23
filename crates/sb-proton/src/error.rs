//! Typed failures and Proton's numeric response-code table
//! (research/proton-api/auth-session.md). No variant carries a
//! credential; the caller classifies the variant, never the text.

use std::fmt;

use crate::auth::HumanVerification;

/// Which sign-in field a refusal applies to (spec/07's sign-in
/// refusals). A wrong password or code is correctable and the
/// conversation continues; the other fields name the account itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidField {
    Password,
    SecondFactor,
    MailboxPassword,
    Account,
    HumanVerification,
}

impl InvalidField {
    /// The spec/07 field name, for the refusal's `field`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::SecondFactor => "second_factor",
            Self::MailboxPassword => "mailbox_password",
            Self::Account => "account",
            Self::HumanVerification => "human_verification",
        }
    }
}

impl fmt::Display for InvalidField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Proton's numeric API codes. Every code the transport recognises is
/// listed with its research citation; unknown codes are refused as
/// `Unavailable` with Proton's message rather than guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiCode {
    /// 1000: the request succeeded (`auth-session.md` §1, §3).
    Success = 1000,
    /// 1001: a partial success (`auth-session.md` §3).
    Multi = 1001,
    /// 2001: an invalid value, e.g. a malformed SRP session
    /// (`auth-session.md` §3).
    InvalidValue = 2001,
    /// 2028: Proton's anti-abuse block, "too many recent logins"
    /// (`auth-session.md` §9).
    AntiAbuse = 2028,
    /// 2064: unknown product or platform in `x-pm-appversion`
    /// (`auth-session.md` §2).
    ProductInvalid = 2064,
    /// 5001: a missing `x-pm-appversion` on Proton's development
    /// server (`policy-risk.md` §2); production answers 5002.
    AppVersionMissingDev = 5001,
    /// 5002: a missing or structurally invalid `x-pm-appversion`
    /// (`auth-session.md` §2).
    AppVersionMissing = 5002,
    /// 5003: an outdated or unsupported app version
    /// (`auth-session.md` §2).
    AppVersionUnsupported = 5003,
    /// 6003: the username is invalid (`go-proton-api/response.go`,
    /// deprecated but still used; not listed in the research
    /// document).
    UsernameInvalid = 6003,
    /// 8002: the password (or, in the 2FA step, the TOTP) is wrong
    /// (`auth-session.md` §4).
    PasswordWrong = 8002,
    /// 9001: human verification (CAPTCHA) is required
    /// (`auth-session.md` §5).
    HumanVerificationRequired = 9001,
    /// 9002: device verification is required (`auth-session.md` §9).
    DeviceVerificationRequired = 9002,
    /// 9101: insufficient scope, e.g. salts on a resumed session
    /// (`auth-session.md` §7).
    InsufficientScope = 9101,
    /// 10004: a paid plan is required (`policy-risk.md` §8).
    PaidPlanRequired = 10004,
    /// 10013: the refresh token is invalid (`auth-session.md` §6).
    RefreshTokenInvalid = 10013,
    /// 12087: the human-verification token was rejected
    /// (`auth-session.md` §5).
    HumanVerificationTokenInvalid = 12087,
}

impl ApiCode {
    /// The code as Proton sends it. Unknown numbers answer `None`.
    pub fn from_i64(code: i64) -> Option<Self> {
        Some(match code {
            1000 => Self::Success,
            1001 => Self::Multi,
            2001 => Self::InvalidValue,
            2028 => Self::AntiAbuse,
            2064 => Self::ProductInvalid,
            5001 => Self::AppVersionMissingDev,
            5002 => Self::AppVersionMissing,
            5003 => Self::AppVersionUnsupported,
            6003 => Self::UsernameInvalid,
            8002 => Self::PasswordWrong,
            9001 => Self::HumanVerificationRequired,
            9002 => Self::DeviceVerificationRequired,
            9101 => Self::InsufficientScope,
            10004 => Self::PaidPlanRequired,
            10013 => Self::RefreshTokenInvalid,
            12087 => Self::HumanVerificationTokenInvalid,
            _ => return None,
        })
    }
}

/// A typed refusal. `Debug` and `Display` never include a credential;
/// `Unavailable::message` is Proton's own error text, which the account
/// health table shows to the user (spec/04).
#[derive(Debug)]
pub enum ProtonError {
    /// The user can correct the named field; the conversation continues.
    InvalidInput(InvalidField),
    /// Proton or the network refused the request: offline, 5xx, an
    /// anti-abuse block, or this app's identity/version being refused.
    Unavailable {
        /// Proton's `Error` text, or a static network description.
        message: String,
    },
    /// 10013: the refresh token was refused; the account needs attention
    /// and the caller must sign in again (spec/04).
    RefreshRefused {
        /// Proton's `Error` text.
        message: String,
    },
    /// 401: the access token was rejected. The caller refreshes once.
    Unauthorized,
    /// The SRP server proof did not match the client's computation;
    /// no token from that response is usable (spec/10).
    ServerProofMismatch,
    /// The modulus signature did not verify with Proton's embedded key;
    /// sign-in aborted before `/auth/v4` (spec/10).
    ModulusSignatureInvalid,
    /// 9001: an authenticated request asked for human verification.
    /// `challenge` carries Proton's details when the response body had
    /// them; `ProtonError::from_api` sees only Proton's `Error` text
    /// and answers `None`. The URL carries a token, so it is never
    /// printed (spec/04 "Human verification during sync", spec/10).
    HumanVerificationRequired {
        challenge: Option<HumanVerification>,
    },
    /// A refusal Proton answered with a numeric API code: an
    /// anti-abuse block, this app's identity/version being refused, a
    /// paid-plan gate, an invalid value, or a device-verification or
    /// scope refusal. The sync worker records the code and message in
    /// the account's diagnostics and maps the account-state refusals
    /// (spec/04 "Account states and health").
    Refused {
        /// Proton's numeric code.
        code: ApiCode,
        /// Proton's `Error` text.
        message: String,
    },
    /// 12087: the human-verification token was rejected.
    HumanVerificationInvalid {
        /// Proton's `Error` text.
        message: String,
    },
    /// The account's only second factor is a FIDO2 security key, which
    /// the pilot does not support (spec/04, `auth-session.md` §9).
    Fido2Only,
    /// No address key unlocked from the key passphrase (spec/04); the
    /// caller names the field (spec/07).
    KeyUnlock(crate::keys::KeyUnlockError),
    /// A response didn't decode; it is a failed request, never a
    /// partial observation (spec/04).
    Malformed {
        /// A static description; never the response body.
        message: String,
    },
    /// A full-message response or a by-id metadata read named a
    /// different message than the one requested. spec/04 step 3: the id
    /// must equal `provider_key`; anything else is refused.
    UnexpectedMessage,
    /// A message response or an attachment data packet grew past its
    /// download bound. It is a typed failure, never a truncated return
    /// (spec/04 "Timeouts").
    DownloadTooLarge,
    /// The request exceeded its deadline: 30 s, or a bounded body
    /// read's whole-body deadline.
    Timeout,
}

impl ProtonError {
    /// A response didn't decode; it is a failed request, never a
    /// partial observation (spec/04 "Fault handling"). The message
    /// carries the path and serde's reason, never the body.
    pub(crate) fn decode(path: &str, error: impl fmt::Display) -> Self {
        Self::Malformed {
            message: format!("the {path} response didn't decode: {error}"),
        }
    }

    /// Maps a numeric Proton code to a typed refusal. `Success` and
    /// `Multi` answer `None`. Each arm cites the research that
    /// establishes the code's meaning.
    pub(crate) fn from_api(code: ApiCode, message: &str) -> Option<Self> {
        Some(match code {
            ApiCode::Success | ApiCode::Multi => return None,
            // `auth-session.md` §9: 6003 names the account, not the
            // password; the caller maps it to `InvalidInput::Account`.
            ApiCode::UsernameInvalid => Self::InvalidInput(InvalidField::Account),
            // `auth-session.md` §4: 8002 is a wrong password; the 2FA
            // step re-labels it `SecondFactor`.
            ApiCode::PasswordWrong => Self::InvalidInput(InvalidField::Password),
            // `auth-session.md` §5: 9001 asks for human verification.
            ApiCode::HumanVerificationRequired => {
                Self::HumanVerificationRequired { challenge: None }
            }
            // `auth-session.md` §5: 12087 is a rejected HV token.
            ApiCode::HumanVerificationTokenInvalid => Self::HumanVerificationInvalid {
                message: message.to_owned(),
            },
            // `auth-session.md` §6: 10013 is a refused refresh token.
            ApiCode::RefreshTokenInvalid => Self::RefreshRefused {
                message: message.to_owned(),
            },
            // Refusals carrying their numeric code, which the sync
            // worker records (spec/04, 10): `auth-session.md` §2
            // 2064/5001/5002/5003 this app's identity, §9 2028 an
            // anti-abuse block, §9 9002 device verification and §7 9101
            // insufficient scope, `policy-risk.md` §8 10004 the
            // paid-plan gate, §3 2001 an invalid value. Listed, not a
            // wildcard, so a new code chooses its mapping.
            code @ (ApiCode::ProductInvalid
            | ApiCode::AppVersionMissing
            | ApiCode::AppVersionMissingDev
            | ApiCode::AppVersionUnsupported
            | ApiCode::AntiAbuse
            | ApiCode::DeviceVerificationRequired
            | ApiCode::InsufficientScope
            | ApiCode::PaidPlanRequired
            | ApiCode::InvalidValue) => Self::Refused {
                code,
                message: message.to_owned(),
            },
        })
    }
}

impl fmt::Display for ProtonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(field) => write!(f, "Proton refused the {field}"),
            Self::Unavailable { message } => write!(f, "{message}"),
            Self::RefreshRefused { message } => {
                write!(f, "Proton refused the refresh token: {message}")
            }
            Self::Unauthorized => f.write_str("Proton rejected the access token"),
            Self::ServerProofMismatch => {
                f.write_str("the SRP server proof did not match; no token was used")
            }
            Self::ModulusSignatureInvalid => {
                f.write_str("the SRP modulus signature did not verify with Proton's key")
            }
            Self::HumanVerificationRequired { .. } => {
                f.write_str("Proton asked for human verification")
            }
            Self::Refused { message, .. } => write!(f, "{message}"),
            Self::HumanVerificationInvalid { message } => {
                write!(f, "Proton rejected the human-verification token: {message}")
            }
            Self::Fido2Only => f.write_str(
                "this account's only second factor is a security key; add an authenticator app \
                 or wait for FIDO2 support",
            ),
            Self::KeyUnlock(error) => write!(f, "{error}"),
            Self::Malformed { message } => write!(f, "a response didn't decode: {message}"),
            Self::UnexpectedMessage => {
                f.write_str("the message response named a different message")
            }
            Self::DownloadTooLarge => f.write_str("the download exceeded its bound"),
            Self::Timeout => f.write_str("the request exceeded its deadline"),
        }
    }
}

impl std::error::Error for ProtonError {}
