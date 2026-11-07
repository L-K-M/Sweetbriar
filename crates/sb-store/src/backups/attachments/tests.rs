use super::*;
use crate::backups::json::tests::allocation;
use std::cell::Cell;

struct OneByte<R>(R);
impl<R: Read> Read for OneByte<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let len = out.len().min(1);
        self.0.read(&mut out[..len])
    }
}

fn expected(input: &str) -> Result<Vec<Reference>, ()> {
    crate::drafts::parse_attachments(input)
        .map_err(|_| ())
        .map(|entries| {
            entries
                .into_iter()
                .map(|e| {
                    let bytes = e.object_sha256.as_bytes();
                    Reference {
                        hash: bytes.try_into().map_or(Hash::Unmatchable, Hash::Bytes),
                        size: e.size,
                    }
                })
                .collect()
        })
}

fn check(input: &str) {
    let want = expected(input);
    for one_byte in [false, true] {
        let mut actual = Vec::new();
        let reader: Box<dyn Read> = if one_byte {
            Box::new(OneByte(input.as_bytes()))
        } else {
            Box::new(input.as_bytes())
        };
        let result = decode(
            reader,
            || Ok(()),
            |reference| {
                actual.push(reference);
                Ok(())
            },
        );
        assert_eq!(
            result.is_ok(),
            want.is_ok(),
            "{input:?}, one_byte={one_byte}, result={result:?}"
        );
        if let Ok(want) = &want {
            assert_eq!(&actual, want, "{input:?}");
        }
    }
}

fn wrapped(fields: &str) -> String {
    format!(r#"[{{"object_sha256":"x","filename":"","media_type":"","size":0,{fields}}}]"#)
}

#[test]
fn canonical_shape_and_duplicate_keys() {
    for text in [
        "[]",
        "null",
        "{}",
        "[null]",
        "[[]]",
        "[{}]",
        "[1]",
        "[true]",
        "[{},]",
        "[]x",
        r#"[{"object_sha256":"x","filename":"","media_type":"","size":0}]"#,
        r#"[{"object_sha256":null,"object_sha256":"x","filename":42,"filename":"a","media_type":{},"media_type":"b","size":false,"size":5}]"#,
        r#"[{"object_sha256":"x","object_sha256":null,"filename":"a","media_type":"b","size":5}]"#,
        r#"[{"object_sha256":"x","filename":"a","media_type":"b","size":5,"size":-0}]"#,
        r#"[{"object_sha256":"x","filename":"a","media_type":"b","size":5,"filename":null}]"#,
        r#"[{"object_\u0073ha256":"x","filename":"a","media_type":"b","size":18446744073709551615}]"#,
        r#"[{"object_sha256":"x","filename":"a","media_type":"b","size":1},{"object_sha256":"y","filename":"b","media_type":"c","size":2}]"#,
    ] {
        check(text);
    }
    check(&wrapped(
        r#""extra":{"a":[null,{"x":true},"\uD83D\uDE00"]}"#,
    ));
    check(&wrapped(r#""object_sha256":{"a":1},"object_sha256":"ok""#));
    for hash in ["a".repeat(64), "é".repeat(32), "b".repeat(65)] {
        check(&wrapped(&format!(r#""object_sha256":"{hash}""#)));
    }
    for value in [
        "1e9999",
        "-1e9999",
        "0e999999999999999999999",
        "1e-999999999999999999999",
        "18446744073709551616",
        "-0",
        "1.0",
        "01",
        "1e",
        "1e+",
        "1.",
        "1x",
        "[1,]",
        r#""\uD800""#,
    ] {
        check(&wrapped(&format!(r#""extra":{value}"#)));
        check(&wrapped(&format!(r#""size":{value}"#)));
        check(&wrapped(&format!(r#""size":{value},"size":7"#)));
    }
}

#[test]
fn number_boundaries_match_serde() {
    for sig in [
        "0",
        "1",
        "17",
        "1234567890123456789",
        "18446744073709551615",
        "18446744073709551616",
        "17976931348623157",
        "17976931348623158",
        "99999999999999999999999999999999999999999",
        "0.0000000000000000000000000000000000000000001",
    ] {
        for exp in [
            -9999, -400, -325, -324, -309, -308, -1, 0, 1, 289, 290, 291, 292, 293, 307, 308, 309,
            400, 9999,
        ] {
            let number = format!("{sig}e{exp}");
            check(&wrapped(&format!(r#""extra":[{number}]"#)));
        }
    }
}

#[test]
fn nesting_and_bad_unicode_match() {
    for depth in [1, 123, 124, 125, 126, 127, 128] {
        let value = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        check(&wrapped(&format!(r#""extra":{value}"#)));
    }
    for bad in [
        r#""\uDC00""#,
        r#""\uD800x""#,
        r#""\x00""#,
        r#""unterminated"#,
    ] {
        check(&wrapped(&format!(r#""extra":{bad}"#)));
    }
}

#[test]
fn long_keys_and_strings_match_the_canonical_reader() {
    let long = "é😀".repeat(20_000);
    check(&wrapped(&format!(
        r#""{long}":"{long}","filename":"{long}","object_sha256":"{long}""#
    )));
    let bytes = b"[{\"object_sha256\":\"\xff\",\"filename\":\"\",\"media_type\":\"\",\"size\":0}]";
    assert!(decode(&bytes[..], || Ok(()), |_| Ok(())).is_err());
}

struct Repeated(usize);
impl Read for Repeated {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = out.len().min(self.0);
        out[..n].fill(b'x');
        self.0 -= n;
        Ok(n)
    }
}

const RECORD: &[u8] =
    br#",{"object_sha256":"x","filename":"","media_type":"","size":0,"extra":1.25e200}"#;
struct Records {
    remaining: usize,
    offset: usize,
}
impl Read for Records {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let n = out.len().min(RECORD.len() - self.offset);
        out[..n].copy_from_slice(&RECORD[self.offset..self.offset + n]);
        self.offset += n;
        if self.offset == RECORD.len() {
            self.offset = 0;
            self.remaining -= 1;
        }
        Ok(n)
    }
}

#[test]
fn large_values_and_many_references_have_bounded_allocations() {
    let large = 3 * 1024 * 1024;
    let input = (&b"[{\""[..])
        .chain(Repeated(large))
        .chain(&b"\":\""[..])
        .chain(Repeated(large))
        .chain(&b"\",\"object_sha256\":\""[..])
        .chain(Repeated(large))
        .chain(&b"\",\"filename\":\""[..])
        .chain(Repeated(large))
        .chain(&b"\",\"media_type\":\"\",\"size\":0}]"[..]);
    let mut count = 0;
    let (result, calls, largest) = allocation::measure(|| {
        decode(
            input,
            || Ok(()),
            |r| {
                assert_eq!(r.hash, Hash::Unmatchable);
                count += 1;
                Ok(())
            },
        )
    });
    result.unwrap();
    assert_eq!(count, 1);
    assert_eq!((calls, largest), (0, 0));

    let first = &br#"[{"object_sha256":"x","filename":"","media_type":"","size":0}"#[..];
    let input = first
        .chain(Records {
            remaining: 20_000,
            offset: 0,
        })
        .chain(&b"]"[..]);
    let mut count = 0;
    let (result, calls, largest) = allocation::measure(|| {
        decode(
            input,
            || Ok(()),
            |_| {
                count += 1;
                Ok(())
            },
        )
    });
    result.unwrap();
    assert_eq!(count, 20_001);
    assert_eq!((calls, largest), (0, 0));
}

#[test]
fn cancellation_io_and_partial_results_are_propagated() {
    #[derive(Debug)]
    struct Marker;
    impl std::fmt::Display for Marker {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("marker")
        }
    }
    impl std::error::Error for Marker {}
    struct Failure;
    impl Read for Failure {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other(Marker))
        }
    }
    let err = decode(Failure, || Ok(()), |_| Ok(())).unwrap_err();
    assert!(err.get_ref().unwrap().is::<Marker>());
    let calls = Cell::new(0);
    let err = decode(
        Repeated(1_000_000),
        || {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                Err(io::Error::other(Marker))
            } else {
                Ok(())
            }
        },
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(err.get_ref().unwrap().is::<Marker>());
    let calls = Cell::new(0);
    let input = (&b"[{\"object_sha256\":\""[..])
        .chain(Repeated(1_000_000))
        .chain(&b"\"}]"[..]);
    let err = decode(
        input,
        || {
            calls.set(calls.get() + 1);
            if calls.get() == 10_000 {
                Err(io::Error::other(Marker))
            } else {
                Ok(())
            }
        },
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(err.get_ref().unwrap().is::<Marker>());
    let text = r#"[{"object_sha256":"x","filename":"","media_type":"","size":0},{}]"#;
    let mut count = 0;
    assert!(
        decode(
            text.as_bytes(),
            || Ok(()),
            |_| {
                count += 1;
                Ok(())
            }
        )
        .is_err()
    );
    assert_eq!(count, 1);
    let err = decode(
        text.as_bytes(),
        || Ok(()),
        |_| Err(io::Error::other(Marker)),
    )
    .unwrap_err();
    assert!(err.get_ref().unwrap().is::<Marker>());
}
