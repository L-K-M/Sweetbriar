//! Tests for [`super`]: the attributes kept, and each budget refusing
//! hostile input instead of exhausting memory or the stack.

use super::*;

/// The elements named `tag`, in document order.
fn elements(tree: &Tree, tag: &str) -> Vec<Handle> {
    let mut found = Vec::new();
    let mut stack = vec![0];
    while let Some(id) = stack.pop() {
        let node = tree.node(id);
        if node.text.is_none() && !node.comment && &*node.name.local == tag {
            found.push(node.clone());
        }
        stack.extend(node.children.borrow().iter().rev().copied());
    }
    found
}

fn text(tree: &Tree) -> String {
    let mut out = String::new();
    let mut stack = vec![0];
    while let Some(id) = stack.pop() {
        let node = tree.node(id);
        if let Some(text) = &node.text {
            out.push_str(&text.borrow());
        }
        stack.extend(node.children.borrow().iter().rev().copied());
    }
    out
}

fn parse(html: &str) -> Tree {
    parse_fragment(html).expect("small HTML parses")
}

#[test]
fn a_fragment_keeps_its_text_comments_and_the_attributes_read() {
    let tree = parse(
        "<!--c--><a href=\"https://e.test\" title=t>x</a><img alt=\"pic\" src=y>\
         <ol start=3><li>i</li></ol><table><tr><td colspan=2 rowspan=x>c</td></tr></table>",
    );
    assert_eq!(text(&tree), "xic");
    assert!(tree.nodes.borrow().iter().any(|node| node.comment));

    let link = &elements(&tree, "a")[0];
    assert_eq!(link.attrs.borrow().href.as_deref(), Some("https://e.test"));
    assert!(!link.attrs.borrow().styling, "title is no styling");
    assert_eq!(
        elements(&tree, "img")[0].attrs.borrow().alt.as_deref(),
        Some("pic")
    );
    assert_eq!(elements(&tree, "ol")[0].attrs.borrow().start, Some(3));
    let cell = elements(&tree, "td")[0].attrs.borrow().clone();
    assert_eq!(
        (cell.colspan, cell.rowspan),
        (2, 1),
        "an unreadable span is 1"
    );
}

#[test]
fn each_styling_attribute_is_noticed() {
    for attribute in [
        "style=x",
        "color=red",
        "bgcolor=red",
        "face=x",
        "size=2",
        "align=left",
    ] {
        let tree = parse(&format!("<span {attribute}>x</span>"));
        assert!(
            elements(&tree, "span")[0].attrs.borrow().styling,
            "{attribute}"
        );
    }
}

#[test]
fn a_reconstructed_formatting_element_keeps_its_link() {
    // The adoption agency algorithm clones `a` across the `p` boundary.
    let tree = parse("<a href=\"https://e.test\">x<p>y</a>z");
    let links = elements(&tree, "a");
    assert!(links.len() >= 2);
    for link in links {
        assert_eq!(link.attrs.borrow().href.as_deref(), Some("https://e.test"));
    }
}

#[test]
fn a_long_ordinary_document_fits_the_budgets() {
    // Formatting elements that close again cost little, however many.
    let unit =
        "<p><span style=x>word</span> <b>bold</b> <a href=\"https://e.test/\">link</a></p>\n";
    let html = unit.repeat(1024 * 1024 / unit.len());
    assert!(parse_fragment(&html).is_ok());
    let rows = "<tr><td>a</td><td><b>b</b></td></tr>".repeat(25_000);
    assert!(parse_fragment(&format!("<table>{rows}</table>")).is_ok());
}

#[test]
fn unclosed_formatting_elements_exhaust_the_work_budget() {
    let html: String = (0..150_000).map(|n| format!("<b x={n}>")).collect();
    assert_eq!(parse_fragment(&html).err(), Some(HtmlLimit::Work));
}

#[test]
fn the_node_budget_refuses_a_flood_of_elements() {
    let html = "<span></span>".repeat(MAX_NODES);
    assert_eq!(parse_fragment(&html).err(), Some(HtmlLimit::Nodes));
}

#[test]
fn the_work_budget_refuses_deep_nesting_without_recursing() {
    let html = format!("{}x", "<div>".repeat(100_000));
    assert_eq!(parse_fragment(&html).err(), Some(HtmlLimit::Work));
}

#[test]
fn a_long_token_of_attributes_is_refused() {
    let names: String = (0..MAX_PENDING_BYTES).map(|n| format!(" a{n}")).collect();
    let html = format!("<a{names}>x</a>");
    assert_eq!(parse_fragment(&html).err(), Some(HtmlLimit::PendingToken));
    let quoted = format!("<a title=\"{}\">x</a>", "x ".repeat(MAX_PENDING_BYTES));
    assert_eq!(parse_fragment(&quoted).err(), Some(HtmlLimit::PendingToken));
    // A `/` outside a value also starts an attribute.
    let slashed: String = (0..MAX_PENDING_BYTES).map(|n| format!("/a{n}")).collect();
    let html = format!("<a{slashed}>x</a>");
    assert_eq!(parse_fragment(&html).err(), Some(HtmlLimit::PendingToken));
}

#[test]
fn a_long_attribute_value_parses() {
    // A pasted screenshot arrives as one base64 `data:` URL.
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let data: String = (0..200_000).map(|i| alphabet[i % 64] as char).collect();
    let tree = parse(&format!(
        "<img src=\"data:image/png;base64,{data}\" alt=p>y"
    ));
    let images = elements(&tree, "img");
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].attrs.borrow().alt.as_deref(), Some("p"));
    assert_eq!(text(&tree), "y");

    let unterminated = format!("<a title=\"{}", "x".repeat(MAX_PENDING_BYTES * 2));
    assert!(parse_fragment(&unterminated).is_ok());
}

#[test]
fn template_contents_stay_inert() {
    let tree = parse("<template><b>x</b></template>y");
    assert!(elements(&tree, "b").is_empty());
    assert_eq!(text(&tree), "y");
}
