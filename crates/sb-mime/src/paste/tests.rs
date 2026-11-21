//! Tests for [`super`] (M4.4e1): one per rule and loss kind.

use super::*;

fn convert(html: &str) -> MarkdownFromHtml {
    markdown_from_html(html).expect("small HTML converts")
}

fn md(html: &str) -> String {
    convert(html).markdown
}

#[test]
fn empty_input_is_empty_and_lossless() {
    let converted = convert("");
    assert_eq!(converted.markdown, "");
    assert!(converted.losses.is_empty());
    assert_eq!(md("<p>   </p>"), "");
}

#[test]
fn oversized_input_is_refused() {
    let html = "a".repeat(MAX_PASTE_HTML_BYTES + 1);
    assert_eq!(markdown_from_html(&html), Err(PasteError::TooLarge));
}

#[test]
fn over_budget_input_is_refused() {
    // 1,995,000 bytes, below the size cap, but far past the node budget.
    let html = "<b></b>".repeat(285_000);
    assert_eq!(markdown_from_html(&html), Err(PasteError::TooComplex));
}

#[test]
fn hostile_nesting_is_walked_iteratively() {
    // Breadth within the node budget converts.
    assert_eq!(md("<div></div>".repeat(100_000).as_str()), "");

    // html5ever's own scope scans are quadratic in depth, so depth past
    // the work budget is refused cleanly instead of overflowing the stack.
    let divs = format!("{}x", "<div>".repeat(100_000));
    assert_eq!(markdown_from_html(&divs), Err(PasteError::TooComplex));
    let quotes = format!("{}x", "<blockquote>".repeat(100_000));
    assert_eq!(markdown_from_html(&quotes), Err(PasteError::TooComplex));
}

#[test]
fn quote_nesting_saturates_at_the_cap() {
    let quotes = format!("{}x", "<blockquote>".repeat(40));
    assert_eq!(md(&quotes), format!("{}x", "> ".repeat(MAX_NESTING)));
}

#[test]
fn comments_metadata_and_hidden_content_are_dropped_silently() {
    let html = "<head><title>t</title></head><style>s</style><script>j</script>\
        <template>t</template><noscript>n</noscript><!--c--><p>a</p>";
    let converted = convert(html);
    assert_eq!(converted.markdown, "a");
    assert!(converted.losses.is_empty());
}

#[test]
fn block_containers_become_paragraphs() {
    let html = "<div>a</div><section>b</section><article>c</article><header>d</header>\
        <footer>e</footer><main>f</main><nav>g</nav><aside>h</aside><p>i</p>";
    assert_eq!(md(html), "a\n\nb\n\nc\n\nd\n\ne\n\nf\n\ng\n\nh\n\ni");
}

#[test]
fn figure_caption_definition_terms_and_addresses_are_paragraphs() {
    assert_eq!(md("<figure>i<figcaption>j</figcaption></figure>"), "i\n\nj");
    assert_eq!(md("<dl><dt>l</dt><dd>m</dd></dl>"), "l\n\nm");
    assert_eq!(md("<address>k</address>"), "k");
}

#[test]
fn headings_use_atx_levels() {
    let html = "<h1>a</h1><h2>b</h2><h3>c</h3><h4>d</h4><h5>e</h5><h6>f</h6>";
    assert_eq!(
        md(html),
        "# a\n\n## b\n\n### c\n\n#### d\n\n##### e\n\n###### f"
    );
}

#[test]
fn a_break_inside_a_heading_is_a_space() {
    // An ATX heading is one line, so a hard break would end it.
    assert_eq!(md("<h1>a<br>b</h1>"), "# a b");
    assert_eq!(md("<h2>a<br><br>b<br></h2>"), "## a b");
}

#[test]
fn blockquotes_prefix_every_line() {
    assert_eq!(md("<blockquote>a</blockquote>"), "> a");
    assert_eq!(
        md("<blockquote><blockquote>a</blockquote></blockquote>"),
        "> > a"
    );
}

#[test]
fn unordered_lists_use_dash_markers() {
    assert_eq!(md("<ul><li>a</li><li>b</li></ul>"), "- a\n- b");
}

#[test]
fn ordered_lists_honour_start_and_number_onward() {
    assert_eq!(
        md("<ol start=\"3\"><li>a</li><li>b</li></ol>"),
        "3. a\n4. b"
    );
    assert_eq!(md("<ol><li>a</li></ol>"), "1. a");
}

#[test]
fn nested_lists_indent_to_the_content_column() {
    assert_eq!(
        md("<ul><li>a<ul><li>b</li></ul></li><li>c</li></ul>"),
        "- a\n  - b\n- c"
    );
    assert_eq!(md("<ul><li>a<br>b</li></ul>"), "- a\\\n  b");
}

#[test]
fn pre_becomes_a_fence_longer_than_any_backtick_run() {
    assert_eq!(
        md("<pre>fn main() {\n    println!(\"x\");\n}</pre>"),
        "```\nfn main() {\n    println!(\"x\");\n}\n```"
    );
    assert_eq!(md("<pre>a `` b ``` c</pre>"), "````\na `` b ``` c\n````");
    assert_eq!(md("<pre></pre>"), "```\n```");
    assert_eq!(md("<pre>  spaced\t</pre>"), "```\n  spaced\n```");
}

#[test]
fn hr_is_three_dashes() {
    assert_eq!(md("<p>a</p><hr><p>b</p>"), "a\n\n---\n\nb");
}

#[test]
fn br_is_a_backslash_hard_break() {
    assert_eq!(md("<p>a<br>b</p>"), "a\\\nb");
    // A break with no following line renders nothing, not a literal slash.
    assert_eq!(md("<p>a<br></p>"), "a");
}

#[test]
fn inline_emphasis_uses_its_markers() {
    assert_eq!(
        md(
            "<p><strong>a</strong> <b>b</b> <em>c</em> <i>d</i> <del>e</del> \
             <s>f</s> <strike>g</strike></p>"
        ),
        "**a** **b** *c* *d* ~~e~~ ~~f~~ ~~g~~"
    );
}

#[test]
fn emphasis_moves_edge_whitespace_out_and_drops_when_empty() {
    assert_eq!(md("<p>x<em> a </em>y</p>"), "x *a* y");
    assert_eq!(md("<p>a<strong> </strong>b</p>"), "ab");
    assert_eq!(md("<p><strong><em>a</em></strong></p>"), "***a***");
}

#[test]
fn inline_code_uses_a_longer_fence_and_pads_backtick_edges() {
    assert_eq!(md("<p><code>a ` b</code></p>"), "``a ` b``");
    assert_eq!(md("<p><code>`a</code></p>"), "`` `a ``");
    // An empty span would render as two literal backticks.
    assert_eq!(md("<p>a<code></code>b</p>"), "ab");
}

#[test]
fn safe_links_keep_their_destination() {
    assert_eq!(
        md("<p><a href=\"https://example.com\">x</a></p>"),
        "[x](https://example.com)"
    );
    assert_eq!(
        md("<p><a href=\"MAILTO:a@b.c\">m</a></p>"),
        "[m](MAILTO:a@b.c)"
    );
    assert_eq!(
        md("<p><a href=\"https://e.com/a b(c)\">x</a></p>"),
        "[x](<https://e.com/a b(c)>)"
    );
    // As a browser reads it: edge whitespace and line breaks drop out.
    assert_eq!(
        md("<p><a href=\" https://e.com/a\nb \">t</a></p>"),
        "[t](https://e.com/ab)"
    );
}

#[test]
fn other_links_keep_only_their_text() {
    let converted = convert("<p><a href=\"javascript:alert(1)\">x</a></p>");
    assert_eq!(converted.markdown, "x");
    assert_eq!(converted.losses, vec![PasteLoss::Links]);

    let converted = convert("<p><a href=\"page.html\">x</a></p>");
    assert_eq!(converted.markdown, "x");
    assert_eq!(converted.losses, vec![PasteLoss::Links]);
}

#[test]
fn links_without_href_or_text_follow_the_special_cases() {
    assert_eq!(md("<p><a>x</a></p>"), "x");
    assert_eq!(
        md("<p><a href=\"https://e.com\"></a></p>"),
        "[https://e.com](https://e.com)"
    );
}

#[test]
fn images_are_dropped_and_keep_only_alt_text() {
    let converted = convert("<p>a<img src=x alt=\"a*b\">c</p>");
    assert_eq!(converted.markdown, "aa\\*bc");
    assert_eq!(converted.losses, vec![PasteLoss::Images]);

    let converted = convert("<p><img src=x></p>");
    assert_eq!(converted.markdown, "");
    assert_eq!(converted.losses, vec![PasteLoss::Images]);
}

#[test]
fn picture_svg_and_media_drop_their_content() {
    let converted = convert("<p>a<picture><img alt=p></picture>b</p>");
    assert_eq!(converted.markdown, "ab");
    assert_eq!(converted.losses, vec![PasteLoss::Images]);

    let converted = convert("<p>a<svg><text>x</text></svg>b</p>");
    assert_eq!(converted.markdown, "ab");
    assert_eq!(converted.losses, vec![PasteLoss::Images]);

    let converted = convert("<p>a<object><b>x</b></object>b</p>");
    assert_eq!(converted.markdown, "ab");
    assert_eq!(converted.losses, vec![PasteLoss::Images]);
}

#[test]
fn form_controls_canvas_and_math_are_other_losses() {
    for html in [
        "<p>a<input value=x>b</p>",
        "<p>a<button>c</button>b</p>",
        "<p>a<select><option>x</option></select>b</p>",
        "<p>a<textarea>x</textarea>b</p>",
        "<p>a<canvas>x</canvas>b</p>",
        "<p>a<math><mi>x</mi></math>b</p>",
    ] {
        let converted = convert(html);
        assert_eq!(converted.markdown, "ab", "{html}");
        assert_eq!(converted.losses, vec![PasteLoss::Other], "{html}");
    }
}

#[test]
fn styling_attributes_and_elements_are_reported() {
    let converted = convert("<p style=\"color:red\">a</p>");
    assert_eq!(converted.markdown, "a");
    assert_eq!(converted.losses, vec![PasteLoss::Styling]);

    let converted = convert(
        "<p><font color=red>b</font> <u>u</u> <mark>m</mark> <sub>s</sub> \
                 <sup>p</sup> <small>z</small> <big>i</big></p>",
    );
    assert_eq!(converted.markdown, "b u m s p z i");
    assert_eq!(converted.losses, vec![PasteLoss::Styling]);
}

#[test]
fn unknown_elements_keep_their_content_without_a_loss() {
    let converted = convert("<p><span>a</span><custom>b</custom></p>");
    assert_eq!(converted.markdown, "ab");
    assert!(converted.losses.is_empty());
}

#[test]
fn markdown_syntax_in_text_is_backslash_escaped() {
    assert_eq!(md("<p>* _ [ ] ~ ` \\</p>"), "\\* \\_ \\[ \\] \\~ \\` \\\\");
    assert_eq!(md("<p>a!b</p>"), "a!b");
    assert_eq!(md("<p>![x</p>"), "\\!\\[x");
    assert_eq!(md("<p>&amp;x</p>"), "&x");
    // Raw HTML shows as its source, escapes included (spec/07).
    assert_eq!(md("<p>&lt;b&gt; &lt;!x</p>"), "\\<b> \\<!x");
    assert_eq!(md("<p>&nosuch;</p>"), "\\&nosuch;");
}

#[test]
fn line_initial_syntax_is_escaped() {
    let html = "<p># a</p><p>&gt; b</p><p>+ c</p><p>- d</p><p>= e</p>\
        <p>1. f</p><p>2) g</p><p>12. h</p>";
    assert_eq!(
        md(html),
        "\\# a\n\n\\> b\n\n\\+ c\n\n\\- d\n\n\\= e\n\n1\\. f\n\n2\\) g\n\n12\\. h"
    );
}

#[test]
fn losses_are_sorted_in_enum_order_and_deduplicated() {
    let html = "<p style=x>a</p><img src=x><a href=\"javascript:x\">l</a><input><u>b</u>";
    let converted = convert(html);
    assert_eq!(converted.markdown, "a\n\nlb");
    assert_eq!(
        converted.losses,
        vec![
            PasteLoss::Images,
            PasteLoss::Styling,
            PasteLoss::Links,
            PasteLoss::Other,
        ]
    );
}

#[test]
fn the_bridge_contract_examples_convert_as_expected() {
    let converted = convert("<h2>Plan</h2><ul><li><em>one</em></li></ul>");
    assert_eq!(converted.markdown, "## Plan\n\n- *one*");
    assert!(converted.losses.is_empty());
}

#[test]
fn blocks_are_separated_by_one_blank_line_and_nothing_trails() {
    let markdown = md("<p>a</p><p>b</p>");
    assert_eq!(markdown, "a\n\nb");
    assert!(!markdown.ends_with('\n'));
    assert_eq!(md("<p>a</p>\n\n<p>b</p>"), "a\n\nb");
}

#[test]
fn the_same_input_always_converts_the_same_way() {
    let html = "<h2>Plan</h2><ul><li><em>one</em></li></ul>";
    assert_eq!(markdown_from_html(html), markdown_from_html(html));
}

#[test]
fn a_quote_keeps_its_marker_between_its_blocks() {
    assert_eq!(
        md("<blockquote><p>a</p><p>b</p></blockquote>"),
        "> a\n>\n> b"
    );
    assert_eq!(
        md("<blockquote>q<ul><li>i</li></ul></blockquote><p>x</p>"),
        "> q\n>\n> - i\n\nx"
    );
    assert_eq!(
        md("<blockquote><blockquote><p>a</p><p>b</p></blockquote></blockquote>"),
        "> > a\n> >\n> > b"
    );
}

#[test]
fn an_items_first_paragraph_stays_on_its_marker_line() {
    assert_eq!(md("<ul><li><p>a</p><p>b</p></li></ul>"), "- a\n\n  b");
    assert_eq!(
        md("<ol><li><div>a</div></li><li><p>b</p></li></ol>"),
        "1. a\n\n2. b"
    );
}

#[test]
fn adjacent_runs_of_one_emphasis_merge() {
    assert_eq!(md("<p><b>a</b><b>b</b></p>"), "**ab**");
    assert_eq!(md("<p><i>a</i><em>b</em> <i>c</i></p>"), "*ab* *c*");
}

#[test]
fn a_fence_inside_a_quote_keeps_its_marker_on_blank_lines() {
    assert_eq!(
        md("<blockquote><pre>a\n\nb</pre></blockquote>"),
        "> ```\n> a\n>\n> b\n> ```"
    );
    assert_eq!(md("<pre>a\n\nb</pre>"), "```\na\n\nb\n```");
    assert_eq!(
        md("<ul><li><pre>a\n\nb</pre></li></ul>"),
        "-\n\n  ```\n  a\n\n  b\n  ```"
    );
}

#[test]
fn a_link_around_blocks_keeps_its_text_and_reports_the_link() {
    let converted = convert("<a href=\"https://x\"><p>Read more</p></a>");
    assert_eq!(converted.markdown, "Read more");
    assert_eq!(converted.losses, vec![PasteLoss::Links]);
    assert_eq!(
        md("<p><a href=\"https://x\">text</a></p>"),
        "[text](https://x)"
    );
    assert_eq!(
        md("<p><a href=\"https://x\"></a></p>"),
        "[https://x](https://x)"
    );
}

#[test]
fn backslashes_and_controls_in_destinations_survive_reparsing() {
    assert_eq!(
        md("<p><a href=\"mailto:a@b\\*c\">x</a></p>"),
        "[x](mailto:a@b\\\\*c)"
    );
    assert_eq!(
        md("<p><a href=\"https://e.com/a b\\c\">x</a></p>"),
        "[x](<https://e.com/a b\\\\c>)"
    );
    assert_eq!(
        md("<p><a href=\"https://e.com/a&#11;b\">x</a></p>"),
        "[x](https://e.com/a%0Bb)"
    );
}

#[test]
fn code_spans_keep_nested_text_without_decoration() {
    let converted = convert("<p><code>a<b>b</b>c</code></p>");
    assert_eq!(converted.markdown, "`abc`");
    assert_eq!(converted.losses, vec![PasteLoss::Styling]);
    let converted = convert("<p><code><a href=\"https://x\">y</a></code></p>");
    assert_eq!(converted.markdown, "`y`");
    assert_eq!(converted.losses, vec![PasteLoss::Links]);
    assert_eq!(md("<p><b>a<code>c</code></b></p>"), "**a`c`**");
}

#[test]
fn inline_frames_around_blocks_keep_their_text_in_order() {
    assert_eq!(md("x<a href=\"https://x\"><p>a</p></a>"), "x\n\na");
    assert_eq!(
        md("<blockquote><a href=\"https://x\"><p>a</p><p>b</p></a></blockquote>"),
        "> a\n>\n> b"
    );
    assert_eq!(
        md("<blockquote>x<a href=\"https://x\"><p>a</p>b</a></blockquote>"),
        "> x\n>\n> a\n>\n> b"
    );
    let converted = convert("<blockquote><b>x<p>a</p>b</b></blockquote>");
    assert_eq!(converted.markdown, "> x\n>\n> a\n>\n> **b**");
    assert_eq!(converted.losses, vec![PasteLoss::Styling]);
    assert_eq!(
        md("<blockquote><i>q<b>x<p>a</p>b</b>z</i></blockquote>"),
        "> qx\n>\n> a\n>\n> ***b**z*"
    );
    assert_eq!(
        md("<ul><li><a href=\"https://x\">a<ul><li>b</li></ul></a></li></ul>"),
        "- a\n  - b"
    );
}

#[test]
fn a_stray_item_or_menu_list_keeps_the_structure() {
    assert_eq!(
        md("<blockquote><li>x</li><p>y</p></blockquote>"),
        "> x\n>\n> y"
    );
    assert_eq!(md("<menu><li>a</li><li>b</li></menu>"), "- a\n- b");
    assert_eq!(md("<dir><li>a</li></dir>"), "- a");
    assert_eq!(md("<div>a</div><header>d</header>e"), "a\n\nd\n\ne");
}

#[test]
fn a_list_number_stays_within_nine_digits() {
    assert_eq!(
        md("<ol start=999999999><li>a</li><li>b</li></ol>"),
        "999999999. a\n999999999. b"
    );
    assert_eq!(
        md("<ol start=9223372036854775807><li>a</li></ol>"),
        "999999999. a"
    );
}

#[test]
fn a_break_inside_emphasis_or_a_link_is_a_hard_break() {
    assert_eq!(md("<p><b>a<br>b</b></p>"), "**a\\\nb**");
    assert_eq!(
        md("<p><a href=\"https://x\">a<br>b</a></p>"),
        "[a\\\nb](https://x)"
    );
    assert_eq!(md("<blockquote><i>a<br>b</i></blockquote>"), "> *a\\\n> b*");
    assert_eq!(md("<p><b>a<br></b>b</p>"), "**a**\\\nb");
    assert_eq!(md("<p><code>a<br>b</code></p>"), "`a b`");
}

#[test]
fn deep_inline_nesting_converts_quickly_or_is_refused() {
    // The parse's work budget refuses depth long before the frames'
    // rebuilding costs anything.
    assert_eq!(
        markdown_from_html(&format!("{}x", "<em>".repeat(10_000))),
        Err(PasteError::TooComplex)
    );
    let started = std::time::Instant::now();
    assert!(markdown_from_html(&format!("{}x", "<em>".repeat(1_000))).is_ok());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}
