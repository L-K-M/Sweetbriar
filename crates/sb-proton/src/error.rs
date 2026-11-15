//! Typed failures and Proton's numeric response-code table
//! (research/proton-api/auth-session.md). No variant carries a
//! credential; the caller classifies the variant, never the text.

use std::fmt;

/// Which sign-in field a refusal applies to (spec/07's sign-in
/// refusals). A wrong password is correctable and the conversation
/// continues; the account field names the account itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidField {
    Password,
    Account,
}

impl fmt::Display for InvalidField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Password => "password",
            Self::Account => "account",
        })
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
    /// 8002: the password is wrong (`auth-session.md` §4).
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
    /// A response didn't decode; it is a failed request, never a
    /// partial observation (spec/04).
    Malformed {
        /// A static description; never the response body.
        message: String,
    },
    /// The request exceeded its 30 s deadline.
    Timeout,
}

impl ProtonError {
    /// Maps a numeric Proton code to a typed refusal. `Success` and
    /// `Multi` answer `None`. Each arm cites the research that
    /// establishes the code's meaning.
    pub(crate) fn from_api(code: ApiCode, message: &str) -> Option<Self> {
        Some(match code {
            ApiCode::Success | ApiCode::Multi => return None,
            // `auth-session.md` §2: unknown product/platform (2064) and
            // an outdated version (5003) are this app's identity being
            // refused; there is no fallback (spec/04).
            ApiCode::ProductInvalid
            | ApiCode::AppVersionMissing
            | ApiCode::AppVersionMissingDev
            | ApiCode::AppVersionUnsupported => Self::Unavailable {
                message: message.to_owned(),
            },
            // `auth-session.md` §9: 2028 is an anti-abuse block, not a
            // user error; stop retrying and show Proton's message.
            ApiCode::AntiAbuse => Self::Unavailable {
                message: message.to_owned(),
            },
            // `auth-session.md` §9: 9002 device verification and §7
            // 9101 insufficient scope are account-state refusals.
            ApiCode::DeviceVerificationRequired | ApiCode::InsufficientScope => Self::Unavailable {
                message: message.to_owned(),
            },
            // `policy-risk.md` §8: 10004 is the paid-plan gate.
            ApiCode::PaidPlanRequired => Self::Unavailable {
                message: message.to_owned(),
            },
            // `auth-session.md` §3: 2001 is an invalid request value;
            // the sign-in conversation treats it as a failed request.
            ApiCode::InvalidValue => Self::Unavailable {
                message: message.to_owned(),
            },
            // `auth-session.md` §9: 6003 names the account, not the
            // password; the caller maps it to `InvalidInput::Account`.
            ApiCode::UsernameInvalid => Self::InvalidInput(InvalidField::Account),
            // `auth-session.md` §4: 8002 is a wrong password.
            ApiCode::PasswordWrong => Self::InvalidInput(InvalidField::Password),
            // `auth-session.md` §5: 9001 and 12087 are the human
            // verification challenge and its rejected token; this
            // transport slice does not drive them.
            ApiCode::HumanVerificationRequired | ApiCode::HumanVerificationTokenInvalid => {
                Self::Unavailable {
                    message: message.to_owned(),
                }
            }
            // `auth-session.md` §6: 10013 is a refused refresh token.
            ApiCode::RefreshTokenInvalid => Self::RefreshRefused {
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
            Self::Malformed { message } => write!(f, "a response didn't decode: {message}"),
            Self::Timeout => f.write_str("the request exceeded its 30 s deadline"),
        }
    }
}

impl std::error::Error for ProtonError {}
