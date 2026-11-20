//! The frozen send message's parts (spec/05 "Proton submission" step
//! 4): the subject, the one body package a recipient needs and the
//! attachments, parsed from the attempt's published bytes with
//! `sb-mime`'s reader helpers. The worker reads the draft's subject,
//! recipients and bodies from the frozen MIME, never from the
//! editable revision.

use sb_mime::{Limits, RenderWarningCode, render_input};

const UNPARSED: &str = "the frozen message doesn't parse";

/// The body package the frozen MIME carries (spec/05 step 4): a plain
/// draft's `text/plain`, a Markdown draft's `text/html` (the plain
/// alternative is not a second package in the pilot).
pub struct FrozenBody {
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// One attachment the frozen MIME carries (spec/05 step 3), as the
/// reader describes it. M4.3e3 uploads these.
pub struct FrozenAttachment {
    pub name: String,
    pub media_type: String,
    pub size: Option<u64>,
}

/// The frozen message's subject, body and attachments (spec/05).
pub struct FrozenSend {
    pub subject: String,
    pub body: FrozenBody,
    pub attachments: Vec<FrozenAttachment>,
}

/// Parse the frozen bytes. The subject comes from the summary and the
/// body and attachments from the reader's selection. A message that
/// doesn't parse, parses only in part, or whose text the reader had to
/// replace is an error, never a partial or altered send. Sweetbriar
/// composed these bytes, so any of them is a defect.
pub fn frozen_send(bytes: &[u8]) -> Result<FrozenSend, String> {
    let limits = Limits::default();
    let subject = sb_mime::parse(bytes, &limits)
        .summary()
        .map(|summary| summary.subject().unwrap_or_default().to_owned())
        .ok_or_else(|| UNPARSED.to_owned())?;
    let input = render_input(bytes, &limits).map_err(|_| UNPARSED.to_owned())?;
    let altered = input.warnings().iter().any(|warning| {
        matches!(
            warning.code(),
            RenderWarningCode::PartialParse | RenderWarningCode::CharsetReplaced
        )
    });
    if altered {
        return Err(UNPARSED.to_owned());
    }
    let (mime_type, data) = if let Some(html) = input.html() {
        ("text/html", html.text().as_bytes().to_vec())
    } else if let Some(plain) = input.plain() {
        ("text/plain", plain.text().as_bytes().to_vec())
    } else {
        return Err("the frozen message has no body".to_owned());
    };
    let attachments = input
        .attachments()
        .iter()
        .map(|attachment| FrozenAttachment {
            name: attachment.display_name().to_owned(),
            media_type: attachment.media_type().to_owned(),
            size: attachment.decoded_length(),
        })
        .collect();
    Ok(FrozenSend {
        subject,
        body: FrozenBody {
            mime_type: mime_type.to_owned(),
            data,
        },
        attachments,
    })
}

#[cfg(test)]
mod tests {
    use sb_mime::{ComposeAddress, ComposeAttachment, ComposeFormat, ComposeInput, compose};

    use super::frozen_send;

    fn composed(format: ComposeFormat, attachments: &[ComposeAttachment<'_>]) -> sb_mime::Composed {
        let to = [ComposeAddress {
            name: None,
            address: "friend@example.test",
        }];
        compose(&ComposeInput {
            from: ComposeAddress {
                name: Some("Owner"),
                address: "owner@example.test",
            },
            to: &to,
            cc: &[],
            bcc: &[],
            reply_to: None,
            subject: "Synthetic subject",
            body: "Hello **there**.",
            format,
            attachments,
            message_id: "00000000-0000-4000-8000-000000000001@example.test",
            date: 1_700_000_000,
            in_reply_to: None,
            references: &[],
            user_agent: "Sweetbriar test",
        })
        .expect("the synthetic draft composes")
    }

    /// The frozen part's text with MIME's CRLF line ends read as the
    /// composer's LF.
    fn text(data: &[u8]) -> String {
        String::from_utf8(data.to_vec())
            .expect("the body is UTF-8")
            .replace("\r\n", "\n")
    }

    #[test]
    fn a_plain_draft_sends_its_plain_text() {
        let message = composed(ComposeFormat::Plain { literal: true }, &[]);
        let frozen = frozen_send(&message.bytes).expect("the frozen message parses");
        assert_eq!(frozen.subject, "Synthetic subject");
        assert_eq!(frozen.body.mime_type, "text/plain");
        assert_eq!(
            text(&frozen.body.data),
            message.plain_text.replace("\r\n", "\n")
        );
        assert!(frozen.attachments.is_empty());
    }

    #[test]
    fn a_markdown_draft_sends_its_html_and_lists_attachments() {
        let attachment = ComposeAttachment {
            name: "notes.txt",
            mime_type: "text/plain",
            data: b"synthetic attachment",
            content_id: None,
        };
        let message = composed(ComposeFormat::Markdown, &[attachment]);
        let frozen = frozen_send(&message.bytes).expect("the frozen message parses");
        assert_eq!(frozen.body.mime_type, "text/html");
        let html = message.html.as_deref().expect("a Markdown draft has HTML");
        assert_eq!(text(&frozen.body.data), html.replace("\r\n", "\n"));
        assert_eq!(frozen.attachments.len(), 1);
        assert_eq!(frozen.attachments[0].name, "notes.txt");
        assert_eq!(frozen.attachments[0].media_type, "text/plain");
        assert_eq!(frozen.attachments[0].size, Some(20));
    }

    #[test]
    fn a_message_without_a_readable_body_is_refused() {
        assert!(frozen_send(b"").is_err());
        let headers_only = b"Subject: x\r\nContent-Type: image/png\r\n\r\nAAAA";
        assert!(frozen_send(headers_only).is_err());
    }
}
