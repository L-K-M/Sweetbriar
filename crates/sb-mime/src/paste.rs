//! HTML-to-Markdown paste conversion (spec/07 "Paste conversion",
//! M4.4e1). This slice is the bounded HTML5 parse; the converter that
//! reads its tree follows in M4.4e1b.

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "M4.4e1b's converter reads the tree")
)]
mod dom;
