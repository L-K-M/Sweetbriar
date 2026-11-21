//! Counted read-only composer services (spec/07).
use crate::{ComposeCatalog, DraftRecipient, Engine, EngineError};
use sb_resources::{
    MAX_PASTE_HTML_BYTES, MAX_RECIPIENT_INPUT_BYTES, MarkdownFromHtml, PasteError,
    RecipientInputError, RecipientQuery,
};

fn invalid(error: RecipientInputError) -> EngineError {
    EngineError::invalid_input(error.field(), error.reason())
}

impl Engine {
    /// Retained identities and a selected-account/global new-draft default.
    pub async fn compose_catalog(
        &self,
        preferred_account_id: Option<&str>,
    ) -> Result<ComposeCatalog, EngineError> {
        if preferred_account_id
            .is_some_and(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
        {
            return Err(EngineError::invalid_input(
                "preferred_account_id",
                "use a nonempty account id of at most 128 UTF-8 bytes without controls",
            ));
        }
        let preferred = preferred_account_id.map(str::to_owned);
        self.read_through(move |r| {
            r.read(|tx| sb_resources::compose_catalog(tx, preferred.as_deref()))
        })
        .await
    }

    /// Literal, bounded history suggestions. Never a network/Contacts query.
    pub async fn recipient_completions(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<DraftRecipient>, EngineError> {
        let query = RecipientQuery::new(prefix, limit).map_err(invalid)?;
        self.read_through(move |r| r.read(|tx| sb_resources::recipient_completions(tx, &query)))
            .await
    }

    /// Strict whole-input parsing on the counted blocking pool, not the UI.
    pub async fn parse_draft_recipients(
        &self,
        text: &str,
    ) -> Result<Vec<DraftRecipient>, EngineError> {
        if text.len() > MAX_RECIPIENT_INPUT_BYTES {
            return Err(EngineError::invalid_input(
                "recipients",
                "use at most 16384 UTF-8 bytes",
            ));
        }
        let text = text.to_owned();
        self.compute(move || sb_resources::parse_draft_recipients(&text).map_err(invalid))
            .await
    }

    /// Paste as Markdown's conversion (spec/07) on the counted blocking
    /// pool: a 2 MiB document is real parsing work. The size is checked
    /// before the input is copied there.
    pub async fn markdown_from_html(&self, html: &str) -> Result<MarkdownFromHtml, EngineError> {
        if html.len() > MAX_PASTE_HTML_BYTES {
            return Err(paste_refused(PasteError::TooLarge));
        }
        let html = html.to_owned();
        self.compute(move || sb_resources::markdown_from_html(&html).map_err(paste_refused))
            .await
    }
}

fn paste_refused(error: PasteError) -> EngineError {
    let reason = match error {
        PasteError::TooLarge => "too large: use at most 2 MiB of UTF-8",
        PasteError::TooComplex => "too complex to convert",
    };
    EngineError::invalid_input("html", reason)
}
