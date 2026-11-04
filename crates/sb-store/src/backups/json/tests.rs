use super::*;
use std::cell::Cell;

struct Chunked<'a> {
    bytes: &'a [u8],
    chunks: &'a [usize],
    turn: usize,
}

impl Read for Chunked<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = out.len().min(self.chunks[self.turn % self.chunks.len()]);
        self.turn += 1;
        self.bytes.read(&mut out[..n])
    }
}

fn parse(input: impl Read) -> io::Result<()> {
    let mut cancel = || Ok(());
    let mut reader = Reader::new(input, &mut cancel);
    reader.skip_value(0)?;
    reader.finish()
}

fn differential(bytes: &[u8]) {
    let expected = serde_json::from_slice::<serde_json::Value>(bytes).is_ok();
    for chunks in [&[1][..], &[2, 1, 7, 3, 8191][..], &[8192][..]] {
        assert_eq!(
            parse(Chunked {
                bytes,
                chunks,
                turn: 0
            })
            .is_ok(),
            expected,
            "input {bytes:?}, chunks {chunks:?}"
        );
    }
}

fn reason(error: &io::Error) -> &Invalid {
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    error.get_ref().unwrap().downcast_ref().unwrap()
}

#[test]
fn tokens_grammar_truncation_and_trailing_input_match_serde() {
    for text in [
        "null",
        "true",
        "false",
        "0",
        "-0",
        "123",
        "-987",
        "0.1",
        "-0.5e+2",
        "1E-2",
        "1e0",
        "1e+9",
        "1e-9",
        "18446744073709551616",
        "1e-9999",
        "1.2345678901234567890123456789012345678901234567890",
        "[]",
        "{}",
        "[true,false,null,0,{},[]]",
        r#"{"a":[{"b":true}],"a":false}"#,
        " \r\n\t[0] \r\n\t",
        "",
        " ",
        "tru",
        "True",
        "NULL",
        "nulll",
        "truex",
        "truefalse",
        "false0",
        "null[",
        "+1",
        "01",
        "-01",
        ".1",
        "1.",
        "1e",
        "1e+",
        "1e-",
        "1e+-2",
        "--1",
        "-",
        "1x",
        "0x1",
        "NaN",
        "Infinity",
        "[1,]",
        "{,}",
        "[,1]",
        "[1 2]",
        "[}",
        "{]",
        "{true:1}",
        "{\"a\"}",
        "{\"a\":}",
        "{\"a\":1,}",
        "{\"a\":1 \"b\":2}",
        "[1",
        "[[]",
        "{}[]",
        "true true",
        "0\u{b}",
        "[1\u{a0}]",
        "/*x*/null",
        "null //x",
    ] {
        differential(text.as_bytes());
    }
    let document = br#" {"escaped\u005fkey":[null,true,false,-12.3e+4,"a\n\ud83d\ude00"],"":{}} "#;
    for end in 0..=document.len() {
        differential(&document[..end]);
    }
    for position in 0..document.len() {
        for byte in [0, b'\t', b'"', b'0', b',', b'}', b']', 0x80, 0xff] {
            let mut changed = document.to_vec();
            changed[position] = byte;
            differential(&changed);
        }
    }
    // JSON grammar has no f64 range cap. Attachment semantics must separately
    // preserve the canonical reader's rejection of unrepresentable numbers.
    for text in ["1e9999", "-1e9999"] {
        assert!(parse(text.as_bytes()).is_ok());
        assert!(serde_json::from_str::<serde_json::Value>(text).is_err());
    }
}

#[test]
fn unicode_escapes_and_controls_match_serde() {
    for text in [
        r#""\"\\\/\b\f\n\r\t\u0000\u007f\u0080\u07ff\u0800\uffff""#,
        r#""\ud800\udc00\uDBFF\uDFFF\uD83d\uDe00""#,
        "\"a\u{7f}é€😀\"",
        r#""\ud800""#,
        r#""\udfff""#,
        r#""\ud800\u0041""#,
        r#""\udc00\ud800""#,
        r#""\ud800\ud800""#,
        r#""\uXYZ0""#,
        r#""\u123""#,
        r#""\u""#,
        r#""\x00""#,
        r#""\v""#,
        r#""\'""#,
        "\"abc\\",
        "\"abc",
    ] {
        differential(text.as_bytes());
        differential(format!("{{{text}:null}}").as_bytes());
    }
    for byte in 0..0x20 {
        differential(&[b'"', byte, b'"']);
        differential(format!(r#""\u{byte:04x}""#).as_bytes());
    }
    for invalid in [
        &[0x80][..],
        &[0xc0, 0xaf],
        &[0xc1, 0xbf],
        &[0xc2],
        &[0xc2, b'a'],
        &[0xe0, 0x80, 0x80],
        &[0xed, 0xa0, 0x80],
        &[0xed, 0xbf, 0xbf],
        &[0xef, 0xbf],
        &[0xf0, 0x80, 0x80, 0x80],
        &[0xf4, 0x90, 0x80, 0x80],
        &[0xf5, 0x80, 0x80, 0x80],
        &[0xff],
        &[0xe2, b'"', 0xac],
    ] {
        let bytes = [b"\"", invalid, b"\""].concat();
        differential(&bytes);
        differential(&[b"{", bytes.as_slice(), b":null}"].concat());
    }
    // Each possible single byte, both in a string and outside one.
    for byte in 0..=255 {
        differential(&[b'"', byte, b'"']);
        differential(&[byte]);
    }
}

#[test]
fn exact_unsigned_refuses_coercion_and_overflow() {
    for (text, expected) in [("0", 0), ("1", 1), ("18446744073709551615", u64::MAX)] {
        for chunks in [&[1][..], &[3, 2, 7][..]] {
            let mut cancel = || Ok(());
            let mut reader = Reader::new(
                Chunked {
                    bytes: text.as_bytes(),
                    chunks,
                    turn: 0,
                },
                &mut cancel,
            );
            assert_eq!(reader.unsigned().unwrap(), expected);
            reader.finish().unwrap();
        }
    }
    for text in [
        "18446744073709551616",
        "99999999999999999999999999999999999999999999",
        "-0",
        "-1",
        "-18446744073709551615",
        "+1",
        "01",
        "0.0",
        "1.5",
        "1e0",
        "1E+0",
        "1e-1",
        "",
        "x",
        "1x",
        "1e",
        "0[",
        "true",
    ] {
        let mut cancel = || Ok(());
        let mut reader = Reader::new(text.as_bytes(), &mut cancel);
        assert!(
            reader.unsigned().and_then(|_| reader.finish()).is_err(),
            "{text}"
        );
    }
    let mut cancel = || Ok(());
    let mut reader = Reader::new(&b"18446744073709551616"[..], &mut cancel);
    assert_eq!(
        reason(&reader.unsigned().unwrap_err()),
        &Invalid::UnsignedInteger
    );
}

#[test]
fn decoded_names_and_fixed_capture() {
    let names = ["", "size", "object_sha256", "é😀"];
    for (text, expected) in [
        (r#""""#, Some(0)),
        (r#""size""#, Some(1)),
        (r#""\u0073i\u007ae""#, Some(1)),
        (r#""object\u005fsha256""#, Some(2)),
        (r#""\u00e9\uD83D\uDE00""#, Some(3)),
        ("\"é😀\"", Some(3)),
        (r#""sizes""#, None),
        (r#""siz""#, None),
        (r#""Size""#, None),
        (r#""size\u0000""#, None),
    ] {
        let mut cancel = || Ok(());
        let mut reader = Reader::new(
            Chunked {
                bytes: text.as_bytes(),
                chunks: &[1],
                turn: 0,
            },
            &mut cancel,
        );
        assert_eq!(reader.name(names).unwrap(), expected);
        reader.finish().unwrap();
    }
    for text in [
        r#""mismatch\ud800""#,
        "\"mismatch\u{1}\"",
        "\"mismatch\u{80}",
    ] {
        let mut cancel = || Ok(());
        assert!(
            Reader::new(text.as_bytes(), &mut cancel)
                .name(names)
                .is_err()
        );
    }
    for (text, expected) in [
        (r#""""#, ""),
        (r#""\u00e9\ud83d\ude00""#, "é😀"),
        (r#""\"\\\/\b\f\n\r\t\u0000""#, "\"\\/\u{8}\u{c}\n\r\t\0"),
    ] {
        let mut out = [0; 32];
        let mut cancel = || Ok(());
        let mut reader = Reader::new(text.as_bytes(), &mut cancel);
        let n = reader.fixed_string(&mut out[..expected.len()]).unwrap();
        assert_eq!(&out[..n], expected.as_bytes());
        reader.finish().unwrap();
        if !expected.is_empty() {
            let mut reader = Reader::new(text.as_bytes(), &mut cancel);
            assert_eq!(
                reason(
                    &reader
                        .fixed_string(&mut out[..expected.len() - 1])
                        .unwrap_err()
                ),
                &Invalid::StringCapacity
            );
        }
    }
    let mut cancel = || Ok(());
    let mut reader = Reader::new(&b"\"abc\""[..], &mut cancel);
    assert_eq!(reader.name([]).unwrap(), None);
}

fn nested(depth: usize, kind: usize) -> String {
    let mut text = String::new();
    for level in 0..depth {
        text.push_str(if (level + kind).is_multiple_of(2) {
            "["
        } else {
            "{\"key\":"
        });
    }
    text.push_str("null");
    for level in (0..depth).rev() {
        text.push(if (level + kind).is_multiple_of(2) {
            ']'
        } else {
            '}'
        });
    }
    text
}

#[test]
fn fixed_stack_matches_effective_serde_depth_including_enclosing_values() {
    for depth in [0, 1, 2, 126, 127, 128, 129] {
        for kind in 0..2 {
            let text = nested(depth, kind);
            differential(text.as_bytes());
            assert_eq!(parse(text.as_bytes()).is_ok(), depth <= MAX_CONTAINERS);
            for enclosing in [0, 1, 2, 126, 127, 128, usize::MAX] {
                let mut cancel = || Ok(());
                let mut reader = Reader::new(text.as_bytes(), &mut cancel);
                let result = reader.skip_value(enclosing);
                assert_eq!(
                    result.is_ok(),
                    depth.saturating_add(enclosing) <= MAX_CONTAINERS
                );
                if let Err(error) = result {
                    assert_eq!(reason(&error), &Invalid::Nesting);
                }
            }
        }
    }
    for (open, close) in [("[", "]"), ("{\"\":", "}")] {
        for depth in [127, 128] {
            differential(format!("{}null{}", open.repeat(depth), close.repeat(depth)).as_bytes());
        }
    }
    // Empty containers count too, and depth is released between siblings.
    differential(format!("{}{}", "[".repeat(127), "]".repeat(127)).as_bytes());
    differential(format!("{}{}", "[".repeat(128), "]".repeat(128)).as_bytes());
    assert!(parse(format!("[{},{}]", nested(126, 0), nested(126, 1)).as_bytes()).is_ok());
}

#[derive(Debug)]
struct Injected;
impl std::fmt::Display for Injected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("injected")
    }
}
impl std::error::Error for Injected {}

#[test]
fn preserves_native_io_and_sink_errors_at_every_boundary() {
    struct Failing<'a> {
        bytes: &'a [u8],
        remaining: usize,
    }
    impl Read for Failing<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, Injected));
            }
            let n = out.len().min(self.remaining).min(3);
            let count = self.bytes.read(&mut out[..n])?;
            self.remaining -= count;
            Ok(count)
        }
    }
    let text = br#" {"\u0061":[false,null,true,-1.5e+2,"\ud83d\ude00"]} "#;
    for boundary in 0..=text.len() {
        let error = parse(Failing {
            bytes: text,
            remaining: boundary,
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.get_ref().unwrap().is::<Injected>());
    }
    let mut cancel = || Ok(());
    let mut reader = Reader::new(&b"\"text\""[..], &mut cancel);
    let error = reader
        .string(|_| Err(io::Error::new(io::ErrorKind::BrokenPipe, Injected)))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert!(error.get_ref().unwrap().is::<Injected>());
}

#[test]
fn cancellation_during_buffered_and_chunked_scanning() {
    for text in [
        format!("{}null", " ".repeat(1000)),
        format!("\"{}\"", "x".repeat(1000)),
        format!("\"{}\"", "\\uD83D\\uDE00".repeat(1000)),
        "1".repeat(1000),
        format!("0.{}", "1".repeat(1000)),
        format!("0e{}", "1".repeat(1000)),
        nested(127, 0),
        format!("{{\"{}\":null}}", "x".repeat(1000)),
    ] {
        for chunks in [&[1][..], &[8192][..]] {
            let calls = Cell::new(0);
            let mut cancel = || {
                calls.set(calls.get() + 1);
                if calls.get() == 40 {
                    Err(io::Error::new(io::ErrorKind::Interrupted, Injected))
                } else {
                    Ok(())
                }
            };
            let mut reader = Reader::new(
                Chunked {
                    bytes: text.as_bytes(),
                    chunks,
                    turn: 0,
                },
                &mut cancel,
            );
            let error = reader.skip_value(0).unwrap_err();
            assert_eq!(calls.get(), 40);
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert!(error.get_ref().unwrap().is::<Injected>());
        }
    }
    let mut cancel = || Err(io::Error::new(io::ErrorKind::Interrupted, Injected));
    assert_eq!(
        Reader::new(io::empty(), &mut cancel)
            .finish()
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
}

// A generated stream avoids hiding an input-sized allocation in the fixture.
struct Repeated<'a> {
    pattern: &'a [u8],
    remaining: usize,
    offset: usize,
}
impl Read for Repeated<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let n = out.len().min(self.pattern.len() - self.offset);
        out[..n].copy_from_slice(&self.pattern[self.offset..self.offset + n]);
        self.offset += n;
        if self.offset == self.pattern.len() {
            self.offset = 0;
            self.remaining -= 1;
        }
        Ok(n)
    }
}

#[test]
fn multi_megabyte_strings_and_keys_use_fixed_memory_without_allocating() {
    for pattern in [&b"x"[..], "é😀".as_bytes(), &b"\\uD83D\\uDE00"[..]] {
        let repeats = 3 * 1024 * 1024 / pattern.len() + 1;
        let input = (&b"{\""[..])
            .chain(Repeated {
                pattern,
                remaining: repeats,
                offset: 0,
            })
            .chain(&b"\":[\""[..])
            .chain(Repeated {
                pattern,
                remaining: repeats,
                offset: 0,
            })
            .chain(&b"\",null]}"[..]);
        let (result, calls, largest) = allocation::measure(|| parse(input));
        result.unwrap();
        assert_eq!((calls, largest), (0, 0));

        let input = (&b"\""[..])
            .chain(Repeated {
                pattern,
                remaining: repeats,
                offset: 0,
            })
            .chain(&b"\""[..]);
        let (result, calls, largest) = allocation::measure(|| {
            let mut cancel = || Ok(());
            let mut reader = Reader::new(input, &mut cancel);
            assert_eq!(reader.buffer.len(), INPUT_BYTES);
            assert!(std::mem::size_of_val(&reader) < INPUT_BYTES + 256);
            let matched = reader.name(["size", "object_sha256"])?;
            reader.finish()?;
            Ok::<_, io::Error>(matched)
        });
        assert_eq!(result.unwrap(), None);
        assert_eq!((calls, largest), (0, 0));
    }
    let consumed = Cell::new(0);
    let mut cancel = || {
        consumed.set(consumed.get() + 1);
        Ok(())
    };
    let input = (&b"\""[..])
        .chain(Repeated {
            pattern: b"x",
            remaining: 4_000_000,
            offset: 0,
        })
        .chain(&b"\""[..]);
    let mut reader = Reader::new(input, &mut cancel);
    assert_eq!(
        reason(&reader.fixed_string(&mut [0; 64]).unwrap_err()),
        &Invalid::StringCapacity
    );
    assert!(consumed.get() < 100);
}

// Test-only allocation instrumentation; production lexer has no unsafe code.
#[allow(unsafe_code)]
pub(in crate::backups) mod allocation {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    thread_local! {
        static TRACK: Cell<bool> = const { Cell::new(false) };
        static CALLS: Cell<usize> = const { Cell::new(0) };
        static LARGEST: Cell<usize> = const { Cell::new(0) };
    }
    struct Meter;
    #[global_allocator]
    static ALLOCATOR: Meter = Meter;
    fn record(size: usize) {
        if TRACK.try_with(Cell::get).unwrap_or(false) {
            CALLS.with(|calls| calls.set(calls.get() + 1));
            LARGEST.with(|largest| largest.set(largest.get().max(size)));
        }
    }
    // SAFETY: all allocation/deallocation is forwarded unchanged to System;
    // counters are allocation-free, thread-local and never inspect pointers.
    unsafe impl GlobalAlloc for Meter {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            record(layout.size());
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            record(layout.size());
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            record(size);
            unsafe { System.realloc(ptr, layout, size) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }
    pub(in crate::backups) fn measure<T>(run: impl FnOnce() -> T) -> (T, usize, usize) {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                TRACK.with(|track| track.set(false));
            }
        }
        CALLS.with(|calls| calls.set(0));
        LARGEST.with(|largest| largest.set(0));
        TRACK.with(|track| track.set(true));
        let reset = Reset;
        let result = run();
        drop(reset);
        (result, CALLS.with(Cell::get), LARGEST.with(Cell::get))
    }
}
