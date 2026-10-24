#![no_main]

use libfuzzer_sys::fuzz_target;
use sb_store::{
    Timestamp,
    query::{
        MAX_QUERY_INPUT_BYTES, Node, ParseCode, ParseContext, Query, QueryError, TextFormatError,
    },
};

const CONTEXTS: &[(&str, i64)] = &[
    ("UTC", 1_767_225_600_000),
    ("Pacific/Apia", 1_767_225_600_000),
    ("invalid/zone", 1_767_225_600_000),
    ("UTC", i64::MIN),
];

// Spec/06 permits only associative flattening and singleton groups, in order.
fn normal(node: &Node) -> Node {
    let nodes = match node {
        Node::And(nodes) | Node::Or(nodes) => nodes,
        Node::Not(child) => return Node::Not(Box::new(normal(child))),
        leaf => return leaf.clone(),
    };
    let mut children = Vec::new();
    for child in nodes.iter().map(normal) {
        match (node, child) {
            (Node::And(_), Node::And(nodes)) | (Node::Or(_), Node::Or(nodes))
                if !nodes.is_empty() =>
            {
                children.extend(nodes);
            }
            (_, child) => children.push(child),
        }
    }
    if children.len() == 1 {
        return children.pop().unwrap();
    }
    match node {
        Node::And(_) => Node::And(children),
        Node::Or(_) => Node::Or(children),
        _ => unreachable!("logical group matched above"),
    }
}

fn canonical_parse(result: Result<Query, QueryError>) -> Option<Query> {
    match result {
        Ok(query) => Some(query),
        Err(QueryError::Store(_)) => None,
        Err(QueryError::Parse(error)) => panic!("canonical query was rejected: {error}"),
    }
}

fn check(query: &Query) {
    assert!(query.canonical_json().len() <= MAX_QUERY_INPUT_BYTES);
    let Some(json) = canonical_parse(Query::parse_json(query.canonical_json())) else {
        return;
    };
    assert_eq!(&json, query);
    let text = match query.canonical_text() {
        Ok(text) => text,
        Err(TextFormatError::Limit) => return,
    };
    assert!(text.len() <= MAX_QUERY_INPUT_BYTES);
    let context = ParseContext::new(CONTEXTS[0].0, Timestamp::from_millis(CONTEXTS[0].1));
    let Some(text) = canonical_parse(Query::parse_text(&text, &context)) else {
        return;
    };
    assert_eq!(text.root(), &normal(query.root()));
}

fn parsed(input: &str, result: Result<Query, QueryError>) {
    if input.len() > MAX_QUERY_INPUT_BYTES {
        assert!(matches!(
            &result,
            Err(QueryError::Parse(error)) if error.code() == ParseCode::Limit
        ));
    }
    match result {
        Ok(query) => check(&query),
        Err(QueryError::Store(_)) => {}
        Err(QueryError::Parse(error)) => {
            assert!(error.start() <= error.end() && error.end() <= input.len());
            assert!(input.is_char_boundary(error.start()));
            assert!(input.is_char_boundary(error.end()));
        }
    }
}

fuzz_target!(|bytes: &[u8]| {
    let Ok(input) = std::str::from_utf8(bytes) else {
        return;
    };
    parsed(input, Query::parse_json(input));
    for &(zone, clock) in CONTEXTS {
        let context = ParseContext::new(zone, Timestamp::from_millis(clock));
        parsed(input, Query::parse_text(input, &context));
    }
});
