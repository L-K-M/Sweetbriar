//! The human verification a sync account is waiting on (spec/04
//! "Human verification during sync"): Proton's challenge and, once the
//! user answered it, the token that rides the worker's first request.
//! Both are held in memory only, like a sign-in conversation; a
//! relaunch, a disconnect or a completed sign-in forgets them
//! (spec/10). Nothing here may print a URL or a token.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use sb_resources::{HumanVerification, SecretString};

/// What one account holds: Proton's challenge before the user answers,
/// or the answer the Verify sheet returned.
enum Held {
    Challenge(HumanVerification),
    Answer {
        token: SecretString,
        token_type: String,
    },
}

/// The held challenges and answers by Proton account id.
#[derive(Default)]
pub(crate) struct ProtonVerifications {
    held: Mutex<HashMap<String, Held>>,
}

impl ProtonVerifications {
    /// Hold the challenge a 9001 carried, replacing whatever the
    /// account held.
    pub(crate) fn hold(&self, account_id: &str, challenge: HumanVerification) {
        self.held
            .lock()
            .expect("the verification lock poisoned")
            .insert(account_id.to_owned(), Held::Challenge(challenge));
    }

    /// Forget the account's challenge or answer.
    pub(crate) fn forget(&self, account_id: &str) {
        self.held
            .lock()
            .expect("the verification lock poisoned")
            .remove(account_id);
    }

    /// The held challenge, cloned for the Verify sheet; none when an
    /// answer, not a challenge, is held.
    pub(crate) fn challenge(&self, account_id: &str) -> Option<HumanVerification> {
        match self
            .held
            .lock()
            .expect("the verification lock poisoned")
            .get(account_id)
        {
            Some(Held::Challenge(challenge)) => Some(challenge.clone()),
            Some(Held::Answer { .. }) | None => None,
        }
    }

    /// Whether a challenge (not an answer) is held for the account.
    pub(crate) fn has_challenge(&self, account_id: &str) -> bool {
        matches!(
            self.held
                .lock()
                .expect("the verification lock poisoned")
                .get(account_id),
            Some(Held::Challenge(_))
        )
    }

    /// Store the answer the Verify sheet returned, replacing whatever
    /// the account held.
    pub(crate) fn answer(&self, account_id: &str, token: SecretString, token_type: String) {
        self.held
            .lock()
            .expect("the verification lock poisoned")
            .insert(account_id.to_owned(), Held::Answer { token, token_type });
    }

    /// Take the answer the worker's next request rides, once; a held
    /// challenge stays.
    pub(crate) fn take_answer(&self, account_id: &str) -> Option<(SecretString, String)> {
        let mut held = self.held.lock().expect("the verification lock poisoned");
        match held.get(account_id) {
            Some(Held::Answer { .. }) => match held.remove(account_id) {
                Some(Held::Answer { token, token_type }) => Some((token, token_type)),
                _ => unreachable!("the entry was an answer"),
            },
            Some(Held::Challenge(_)) | None => None,
        }
    }
}

impl fmt::Debug for ProtonVerifications {
    /// Only which accounts hold something: never a URL or a token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let held = self.held.lock().expect("the verification lock poisoned");
        f.debug_struct("ProtonVerifications")
            .field("accounts", &held.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// The challenge `proton_verification(id)` reports (spec/07), as the
/// sign-in sheet's `NeedsHumanVerification`. The URL carries Proton's
/// token, so `Debug` redacts it (spec/10).
#[derive(Clone, PartialEq, Eq)]
pub struct ProtonVerificationChallenge {
    pub url: String,
    pub methods: Vec<String>,
}

impl fmt::Debug for ProtonVerificationChallenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProtonVerificationChallenge")
            .field("url", &"[redacted]")
            .field("methods", &self.methods)
            .finish()
    }
}

/// The answer `verify_proton_account(key, id, answer)` takes: the token
/// the verification page handed back and its type. The token is a
/// secret, so `Debug` redacts it (spec/10).
#[derive(Clone, PartialEq, Eq)]
pub struct ProtonVerificationAnswer {
    pub token: String,
    pub token_type: String,
}

impl fmt::Debug for ProtonVerificationAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProtonVerificationAnswer")
            .field("token", &"[redacted]")
            .field("token_type", &self.token_type)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::*;

    fn challenge(url: &str) -> HumanVerification {
        HumanVerification {
            url: url.to_owned(),
            methods: vec!["captcha".to_owned()],
        }
    }

    #[test]
    fn a_held_challenge_is_reported_and_replaced_by_an_answer() {
        let verifications = ProtonVerifications::default();
        assert!(!verifications.has_challenge("a"));

        verifications.hold("a", challenge("https://verify.proton.me/?token=secret"));
        assert!(verifications.has_challenge("a"));
        let held = verifications.challenge("a").expect("the challenge");
        assert_eq!(held.methods, ["captcha"]);
        assert!(held.url.contains("token=secret"));
        assert!(verifications.take_answer("a").is_none());

        verifications.answer(
            "a",
            SecretString::from("answer-token"),
            "captcha".to_owned(),
        );
        assert!(!verifications.has_challenge("a"), "the answer replaced it");
        assert!(verifications.challenge("a").is_none());
        let taken = verifications.take_answer("a").expect("the answer");
        assert_eq!(taken.0.expose_secret(), "answer-token");
        assert_eq!(taken.1, "captcha");
    }

    #[test]
    fn taking_the_answer_removes_it_once() {
        let verifications = ProtonVerifications::default();
        verifications.answer("a", SecretString::from("one"), "captcha".to_owned());
        assert!(verifications.take_answer("a").is_some());
        assert!(verifications.take_answer("a").is_none());
        assert!(!verifications.has_challenge("a"));
    }

    #[test]
    fn forgetting_drops_a_challenge_or_answer() {
        let verifications = ProtonVerifications::default();
        verifications.hold("a", challenge("https://verify.proton.me/?token=secret"));
        verifications.answer("b", SecretString::from("answer"), "captcha".to_owned());

        verifications.forget("a");
        verifications.forget("b");
        assert!(verifications.challenge("a").is_none());
        assert!(verifications.take_answer("b").is_none());
    }

    #[test]
    fn debug_prints_neither_url_nor_token() {
        let verifications = ProtonVerifications::default();
        verifications.hold(
            "account-1",
            challenge("https://verify.proton.me/?token=secret-url"),
        );
        verifications.answer(
            "account-2",
            SecretString::from("secret-token"),
            "captcha".to_owned(),
        );
        let debug = format!("{verifications:?}");
        assert!(!debug.contains("secret-url"), "{debug}");
        assert!(!debug.contains("secret-token"), "{debug}");

        let challenge = ProtonVerificationChallenge {
            url: "https://verify.proton.me/?token=secret-url".to_owned(),
            methods: vec!["captcha".to_owned()],
        };
        assert!(!format!("{challenge:?}").contains("secret-url"));
        let answer = ProtonVerificationAnswer {
            token: "secret-token".to_owned(),
            token_type: "captcha".to_owned(),
        };
        assert!(!format!("{answer:?}").contains("secret-token"));
    }
}
