//! Preserve exact UID FETCH sets across the pinned codec's automatic
//! sequence normalization, which rewrites the largest numeric UID as
//! '*'. The codec still owns every other part of the command encoding.
use crate::ImapError;
use io_imap::codec::{CommandCodec, encode::Encoder};
use io_imap::types::{
    command::{Command, CommandBody},
    sequence::{SeqOrUid, Sequence},
};

pub(crate) fn encode(command: &Command<'_>) -> Result<Vec<u8>, ImapError> {
    let CommandBody::Fetch {
        sequence_set,
        uid: true,
        ..
    } = &command.body
    else {
        return Ok(CommandCodec::new().encode(command).dump());
    };
    let exact = sequence_set
        .0
        .as_ref()
        .iter()
        .map(|sequence| match sequence {
            Sequence::Single(value) => number(*value),
            Sequence::Range(first, last) => format!("{}:{}", number(*first), number(*last)),
        })
        .collect::<Vec<_>>()
        .join(",");
    let mut placeholder = command.clone();
    let CommandBody::Fetch { sequence_set, .. } = &mut placeholder.body else {
        unreachable!()
    };
    *sequence_set = vec![Sequence::Single(SeqOrUid::Value(std::num::NonZeroU32::MIN))]
        .try_into()
        .expect("one placeholder UID");
    let mut bytes = CommandCodec::new().encode(&placeholder).dump();
    let offset = command.tag.inner().len() + b" UID FETCH ".len();
    if bytes.get(offset..offset + 2) != Some(b"1 ") {
        return Err(ImapError::Malformed);
    }
    // Replace only the known placeholder token before the item list.
    // User strings, literals and tags never participate in this splice.
    bytes.splice(offset..offset + 1, exact.bytes());
    Ok(bytes)
}

fn number(value: SeqOrUid) -> String {
    match value {
        SeqOrUid::Value(number) => number.to_string(),
        SeqOrUid::Asterisk => "*".into(),
    }
}
