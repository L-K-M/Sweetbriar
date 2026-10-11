# Topic: Building a modern, fast, native macOS email UI in Swift (macOS 26 era)

Questions:
1. AppKit vs SwiftUI in 2026: maturity of SwiftUI on macOS 26 (Tahoe)
   for a large productivity app, Liquid Glass design language, known
   performance problems (List/Table with huge data, NSHostingView overhead,
   Observation framework), and how serious Mac apps mix both (examples and
   engineering write-ups). Recommend a split.
2. Message list: NSTableView/NSOutlineView vs SwiftUI Table/List for
   100k+ rows with variable row heights, threading disclosure, multiple
   selection, drag and drop, type-select, keyboard navigation.
3. Message viewer: WKWebView configuration for mail (JavaScript disabled,
   WKContentRuleList for blocking remote content, custom URL schemes for
   cid: images, process pooling and prewarming, dark mode adaptation of
   HTML mail, printing, find-in-page, text selection), and a native fast
   path for plain-text mail (TextKit 2).
4. Composer: rich text vs plain text vs Markdown composing; NSTextView /
   TextKit 2 vs a web-based editor; format=flowed; spell/grammar, Writing
   Tools integration, attachments, inline images, signatures, quoting.
5. macOS integration: App Intents / Shortcuts, AppleScript (Cocoa
   scripting / .sdef) for power users, Services menu, Spotlight (Core
   Spotlight), Quick Look, Share extension, notifications with actions,
   Focus filters, Handoff, default mail app registration (mailto:),
   Menu bar extra, keyboard shortcuts customization, accessibility
   (VoiceOver), localization, Apple Foundation Models framework.
6. Distribution: Mac App Store (sandbox constraints for mail storage,
   AppleScript, plug-ins, background agents) vs Developer ID +
   notarization (Sparkle updates); what Mimestream, MailMate, Spark,
   Airmail do.
7. Examples of exemplary modern native Mac apps to learn from (e.g.
   Things, NetNewsWire, Nova, Tower, Ivory, Mimestream) and what makes
   them feel native and fast.
8. Recommend the UI technology stack and app structure for Sweetbriar.
