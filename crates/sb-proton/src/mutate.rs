//! Proton flag mutations (plan M3.1a): read/unread and label/unlabel
//! with their per-message response codes (spec/04 "Intents and the
//! mutation matrix"; research `proton-api/mail-sync.md` §6). The
//! executor, not the transport, reconciles a retry: the call uses the
//! [`Replies::NoReplay`] policy, so a lost reply, a deadline or a 5xx
//! is returned rather than sent again (`http` considers PUT
//! idempotent, so the default policy would replay it), while pacing,
//! the deadline and 429/503 `Retry-After` backoff stay as for every
//! other call (spec/04 "Connections and pacing").

use std::fmt;

use bytes::Bytes;
use hyper::{Method, StatusCode};
use serde::Deserialize;
use serde_json::json;

use crate::auth::Session;
use crate::client::{
    Client, Priority, Replies, TransportFailure, code_of, error_message, read_response,
};
use crate::error::ProtonError;
use crate::mail::validate_query_id;

/// Proton's Starred system label (research `mail-sync.md` §1).
pub const STARRED_LABEL_ID: &str = "10";

const READ_PATH: &str = "/mail/v4/messages/read";
const UNREAD_PATH: &str = "/mail/v4/messages/unread";
const LABEL_PATH: &str = "/mail/v4/messages/label";
const UNLABEL_PATH: &str = "/mail/v4/messages/unlabel";

/// One message flag change (spec/04's mutation matrix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageMutation {
    /// `PUT /mail/v4/messages/read`: the message becomes read in every
    /// mailbox it appears in.
    Read,
    /// `PUT /mail/v4/messages/unread`: the message becomes unread.
    Unread,
    /// `PUT /mail/v4/messages/label`: adds `label` to the message.
    /// Starred is [`STARRED_LABEL_ID`]; any label id is accepted, for
    /// M5's later use.
    Label(String),
    /// `PUT /mail/v4/messages/unlabel`: removes `label` from the
    /// message.
    Unlabel(String),
}

impl MessageMutation {
    /// The route and JSON body (`go-proton-api/message_types.go:189-196`:
    /// `MessageActionReq{IDs}` and `LabelMessagesReq{LabelID, IDs}`).
    fn request(self, id: &str) -> (&'static str, Vec<u8>) {
        let (path, body) = match self {
            Self::Read => (READ_PATH, json!({"IDs": [id]})),
            Self::Unread => (UNREAD_PATH, json!({"IDs": [id]})),
            Self::Label(label) => (LABEL_PATH, json!({"LabelID": label, "IDs": [id]})),
            Self::Unlabel(label) => (UNLABEL_PATH, json!({"LabelID": label, "IDs": [id]})),
        };
        (
            path,
            serde_json::to_vec(&body).expect("a mutation body serializes"),
        )
    }
}

/// A mutation failure, typed so the M3.1 executor maps it without
/// string matching (spec/04 "Intents and the mutation matrix"). Like
/// [`crate::SendTransportError`], `Debug` and `Display` carry no body
/// text, key material or credentials (spec/10).
#[derive(Debug)]
pub enum MutationError {
    /// Proton answered and refused: HTTP 4xx other than 429, or a
    /// non-success top-level or per-message code. The status stays, so
    /// the caller can tell a 4xx refusal from the other classes.
    Refused {
        /// The HTTP status Proton answered with.
        status: u16,
        /// Proton's numeric code, when it gave one.
        code: Option<i64>,
        /// Proton's `Error` text, or a static description.
        message: String,
    },
    /// The connection was never established (refused, DNS, connect
    /// timeout, failed TLS handshake): nothing was sent, so the
    /// executor retries the intent.
    NeverEstablished {
        /// A static description of the connect failure.
        message: String,
    },
    /// The request may have reached Proton and its outcome is unknown:
    /// a connection lost after the request began, a deadline, a 5xx the
    /// client gave up on, or an unreadable reply. The executor
    /// reconciles from the metadata before retrying (plan M3.1a).
    OutcomeUnknown {
        /// A static description.
        message: String,
    },
    /// 429/503 with the `Retry-After` backoff exhausted before an
    /// answer: nothing was applied, retry later (spec/04).
    Throttled {
        /// A static description.
        message: String,
    },
    /// A pre-request failure with its typed form: 401 refresh needed,
    /// the client closed.
    PreRequest(ProtonError),
    /// A caller-supplied id or label id that cannot be sent.
    InvalidInput {
        /// A static description.
        message: String,
    },
}

impl MutationError {
    /// Maps the client's classified failure. A 5xx the client gave up
    /// on and a lost connection both mean the request reached Proton,
    /// so they are unknown outcomes; a connect failure proves nothing
    /// was sent, and an exhausted rate limit is a throttle.
    fn from_transport(failure: TransportFailure) -> Self {
        match failure {
            TransportFailure::NeverEstablished { message } => Self::NeverEstablished { message },
            TransportFailure::OutcomeUnknown { message } => Self::OutcomeUnknown { message },
            TransportFailure::Timeout => Self::OutcomeUnknown {
                message: "the request exceeded its deadline".to_owned(),
            },
            TransportFailure::RateLimited { message } => Self::Throttled { message },
            TransportFailure::ServerRefused { status, message } => Self::OutcomeUnknown {
                message: format!("Proton answered {status}: {message}"),
            },
            TransportFailure::Malformed { message } => Self::InvalidInput { message },
            TransportFailure::Closed { message } => {
                Self::PreRequest(ProtonError::Unavailable { message })
            }
        }
    }
}

impl fmt::Display for MutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused {
                status,
                code,
                message,
            } => write!(
                f,
                "Proton refused the mutation with {status} (code {code:?}): {message}"
            ),
            Self::NeverEstablished { message } => {
                write!(f, "the connection was never established: {message}")
            }
            Self::OutcomeUnknown { message } => {
                write!(f, "the mutation's outcome is unknown: {message}")
            }
            Self::Throttled { message } => {
                write!(f, "Proton asked us to slow down: {message}")
            }
            Self::PreRequest(error) => write!(f, "{error}"),
            Self::InvalidInput { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for MutationError {}

impl Client {
    /// Applies one message flag change (spec/04 "Intents and the
    /// mutation matrix"), one message per call. The PUT is not replayed
    /// by the transport: a lost reply is
    /// [`MutationError::OutcomeUnknown`] and the executor reconciles
    /// from the metadata (plan M3.1a).
    pub async fn mutate_message(
        &self,
        session: &Session,
        id: &str,
        mutation: MessageMutation,
    ) -> Result<(), MutationError> {
        validate_query_id(id, "a mutation's message id").map_err(|error| {
            MutationError::InvalidInput {
                message: error.to_string(),
            }
        })?;
        if let MessageMutation::Label(label) | MessageMutation::Unlabel(label) = &mutation {
            validate_query_id(label, "a mutation's label id").map_err(|error| {
                MutationError::InvalidInput {
                    message: error.to_string(),
                }
            })?;
        }
        let (path, body) = mutation.request(id);
        let answer = self
            .send_classified(
                Method::PUT,
                path,
                Some(Bytes::from(body)),
                Priority::Background,
                Some((session.uid(), session.expose_access_token())),
                &[],
                Replies::NoReplay,
            )
            .await
            .map_err(MutationError::from_transport)?;
        let (status, bytes) = read_response(answer).await.map_err(|error| {
            // The request reached Proton and the reply began; a body
            // that doesn't read is an unknown outcome, never a partial
            // observation (spec/04 "Fault handling").
            MutationError::OutcomeUnknown {
                message: error.to_string(),
            }
        })?;
        classify_reply(path, status, id, &bytes)
    }
}

/// Classifies a mutation reply: success (an empty 2xx, top-level 1000,
/// or the multi-response 1001 with the message's own 1000), a refusal carrying
/// Proton's status and code, or an unknown outcome. Pure over the
/// status and body so the response rules are unit-testable.
fn classify_reply(
    path: &str,
    status: StatusCode,
    id: &str,
    body: &[u8],
) -> Result<(), MutationError> {
    if status == StatusCode::UNAUTHORIZED {
        return Err(MutationError::PreRequest(ProtonError::Unauthorized));
    }
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        return Err(MutationError::Throttled {
            message: error_message(body),
        });
    }
    if status.is_client_error() {
        return Err(MutationError::Refused {
            status: status.as_u16(),
            code: code_of(body),
            message: error_message(body),
        });
    }
    if !status.is_success() {
        return Err(MutationError::OutcomeUnknown {
            message: format!("Proton answered {status}"),
        });
    }
    // go-proton-api's server answers all four calls with an empty 200
    // (`server/messages.go:196-314`); with nothing refused, the
    // executor's confirming read decides (spec/04 "Running an intent").
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    let reply: MutationReply =
        serde_json::from_slice(body).map_err(|error| MutationError::OutcomeUnknown {
            message: ProtonError::decode(path, error).to_string(),
        })?;
    match reply.code {
        Some(1000) => Ok(()),
        Some(1001) => {
            let own = reply
                .responses
                .iter()
                .find(|response| response.id == id)
                .and_then(|response| response.response.as_ref());
            match own.and_then(|response| response.code) {
                Some(1000) => Ok(()),
                code => Err(MutationError::Refused {
                    status: status.as_u16(),
                    code,
                    message: own
                        .and_then(|response| response.error.clone())
                        .unwrap_or_else(|| format!("the reply named no success for message {id}")),
                }),
            }
        }
        code => Err(MutationError::Refused {
            status: status.as_u16(),
            code,
            message: error_message(body),
        }),
    }
}

/// The label/unlabel reply (`go-proton-api/message_types.go:198-216`:
/// `LabelMessagesRes.Responses[].Response`).
#[derive(Deserialize)]
struct MutationReply {
    #[serde(rename = "Code", default)]
    code: Option<i64>,
    #[serde(rename = "Responses", default)]
    responses: Vec<MutationMessageReply>,
}

#[derive(Deserialize)]
struct MutationMessageReply {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Response", default)]
    response: Option<MutationMessageResponse>,
}

#[derive(Deserialize)]
struct MutationMessageResponse {
    #[serde(rename = "Code", default)]
    code: Option<i64>,
    #[serde(rename = "Error", default)]
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "msg-0";

    #[test]
    fn an_empty_success_is_success() {
        assert!(classify_reply(READ_PATH, StatusCode::OK, ID, b"").is_ok());
        assert!(classify_reply(LABEL_PATH, StatusCode::OK, ID, b"\n").is_ok());
    }

    #[test]
    fn a_top_level_success_code_is_success() {
        assert!(classify_reply(READ_PATH, StatusCode::OK, ID, br#"{"Code":1000}"#).is_ok());
    }

    #[test]
    fn a_multi_response_with_the_message_s_own_success_is_success() {
        let body =
            format!(r#"{{"Code":1001,"Responses":[{{"ID":"{ID}","Response":{{"Code":1000}}}}]}}"#);
        assert!(classify_reply(READ_PATH, StatusCode::OK, ID, body.as_bytes()).is_ok());
    }

    #[test]
    fn a_multi_response_with_a_per_message_failure_is_refused_with_that_code() {
        let body = format!(
            r#"{{"Code":1001,"Responses":[{{"ID":"{ID}","Response":{{"Code":2001,"Error":"message not found"}}}}]}}"#
        );
        let error = classify_reply(READ_PATH, StatusCode::OK, ID, body.as_bytes())
            .expect_err("the per-message code refuses");
        assert!(
            matches!(
                error,
                MutationError::Refused {
                    status: 200,
                    code: Some(2001),
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(error.to_string().contains("message not found"));
    }

    #[test]
    fn a_top_level_failure_code_is_refused() {
        let error = classify_reply(
            READ_PATH,
            StatusCode::BAD_REQUEST,
            ID,
            br#"{"Code":2001,"Error":"invalid value"}"#,
        )
        .expect_err("the code refuses");
        assert!(
            matches!(
                error,
                MutationError::Refused {
                    status: 400,
                    code: Some(2001),
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn http_refusals_keep_their_status() {
        for status in [StatusCode::UNPROCESSABLE_ENTITY, StatusCode::NOT_FOUND] {
            let error = classify_reply(READ_PATH, status, ID, br#"{"Code":2001,"Error":"no"}"#)
                .expect_err("the status refuses");
            assert!(
                matches!(error, MutationError::Refused { status: s, .. } if s == status.as_u16()),
                "{error:?}"
            );
        }
    }

    #[test]
    fn malformed_json_is_an_unknown_outcome() {
        // The request reached Proton, so an unreadable reply is never a
        // partial observation (spec/04 "Fault handling").
        let error = classify_reply(READ_PATH, StatusCode::OK, ID, b"not json")
            .expect_err("the body doesn't decode");
        assert!(
            matches!(error, MutationError::OutcomeUnknown { .. }),
            "{error:?}"
        );
        assert!(!error.to_string().contains("not json"));
    }

    #[test]
    fn unauthorized_and_throttled_keep_their_classes() {
        let error = classify_reply(READ_PATH, StatusCode::UNAUTHORIZED, ID, b"")
            .expect_err("401 is a pre-request failure");
        assert!(
            matches!(error, MutationError::PreRequest(ProtonError::Unauthorized)),
            "{error:?}"
        );
        let error = classify_reply(
            READ_PATH,
            StatusCode::TOO_MANY_REQUESTS,
            ID,
            br#"{"Code":2028,"Error":"slow down"}"#,
        )
        .expect_err("429 is a throttle");
        assert!(
            matches!(error, MutationError::Throttled { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn transport_failures_map_like_the_send_lane_with_a_throttle() {
        assert!(matches!(
            MutationError::from_transport(TransportFailure::RateLimited {
                message: "slow down".to_owned()
            }),
            MutationError::Throttled { .. }
        ));
        assert!(matches!(
            MutationError::from_transport(TransportFailure::ServerRefused {
                status: StatusCode::BAD_GATEWAY,
                message: "boom".to_owned()
            }),
            MutationError::OutcomeUnknown { .. }
        ));
        assert!(matches!(
            MutationError::from_transport(TransportFailure::Timeout),
            MutationError::OutcomeUnknown { .. }
        ));
        assert!(matches!(
            MutationError::from_transport(TransportFailure::NeverEstablished {
                message: "refused".to_owned()
            }),
            MutationError::NeverEstablished { .. }
        ));
        assert!(matches!(
            MutationError::from_transport(TransportFailure::Closed {
                message: "closed".to_owned()
            }),
            MutationError::PreRequest(ProtonError::Unavailable { .. })
        ));
        assert!(matches!(
            MutationError::from_transport(TransportFailure::Malformed {
                message: "bad".to_owned()
            }),
            MutationError::InvalidInput { .. }
        ));
    }
}
