# Sweetbriar analysis

Source baseline was `19fde48e` on 2026-11-26. This review covers the native shell, reader, composer, list and search, sidebar, archive paths, and the qualification records. It uses synthetic test roots. Recorded performance results include their original commits. A live pass in the isolated macOS build at `241250fb` covered first launch, importing a two-message synthetic mbox, selecting a message, and composing a new local draft in Dark Mode. Narrow panes, other appearances, and real-account qualification remain open.

## Correctness and interaction

### Restore the preceding selection when clearing Search

High priority. The field remembers the previous window spec, but opening it creates an empty selection. Capture occurrence IDs when Search first replaces the list. Restore surviving IDs when the matching list is ready and use a bounded lookup for a scroll anchor outside the first page. Test Escape and the clear button, a selected message removed during Search, intervening sidebar navigation, and returning keyboard focus to the list. [Search contract](spec/08-macos-app.md#search-m16), [Search field model](apps/macos/SweetbriarBridge/SearchFieldModel.swift#L85), [List install](apps/macos/Sweetbriar/MessageListViewController.swift#L295)

### Refresh tags and notes without reloading the reader body

High priority. The model preserves the body during metadata refresh, but the controller calls its full render after either write, redoing TextKit styling or starting another WebKit navigation. Refresh only metadata for the current message. Test a note edit and tag add/remove while scrolled deep into HTML and native bodies. Preserve scroll position and selected text, surface write errors, and discard late UI completions from another selection. [Metadata writes](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L593), [Model refresh](apps/macos/SweetbriarBridge/ReaderModel.swift#L202)

### Scope the tag input to one message

Medium priority. Updating the tag chips leaves unsubmitted text in the shared entry field. Clear the draft as reader selection changes and keep it during metadata refresh for the same message. Test entering a tag on A, opening B with the mouse and keyboard, and checking the input before any edit on B. [Tag picker](apps/macos/Sweetbriar/Reader/ReaderMetadataViews.swift#L62)

### Apply Reading view sender preferences in click order

Medium priority. Membership is checked before the settings write returns. A toggle off can see the old membership and return while a toggle on is still pending. Two senders can also start from the same array. Queue edits in the shared memory and apply each against the last committed set. Preserve immediate rendition switching. With a gated source, test on/off during a pending write, two senders across windows, a write failure, and archive retirement during a write. [Sender preference model](apps/macos/SweetbriarBridge/ReadingViewMemory.swift#L48)

## Native layout and accessibility

### Give the reader body a consistent share of the window

High priority. In the live Dark Mode reader, the tag field and empty note occupied two rows before the rendition controls and body. The header also stacks full addresses, account, attachments and status above the scrolling body. The horizontal tag list also has a width cap without dedicated overflow. Keep subject, sender address and account visible first. Put long recipient lists, numerous attachments, and optional tags and notes behind accessible disclosure. Test 20 attachments, 12 tags and long addresses at the minimum pane width. Require a readable first body screen, a reachable attachment action, and VoiceOver access to every disclosed detail. [Reader layout](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L132)

### Make the first-run and empty states actionable

Medium priority. The live first-launch window showed both “No messages” and “No message selected,” with neither import nor sign-in available in either pane. Put Import Mail or Add Proton Account in the empty archive flow. Distinguish an empty mailbox, a reader without a selection, and a zero-result query with incomplete coverage. Use one primary action and concise text. Test clean roots, an empty mailbox beside a populated mailbox, and an incomplete zero-hit search. [Current list states](apps/macos/Sweetbriar/MessageListViewController.swift#L565)

### Establish shared density and typography for the three-pane window

Medium priority. Keep native sidebar selection and visible account cues. Prototype a consistent subject/sender hierarchy, alignment and control sizing, and a readable maximum line length in native reader renditions. Compare the same synthetic messages in light and dark appearance and at narrow and wide sizes. Test Increase Contrast, Full Keyboard Access, long names and scan speed. Use measured text extents before changing table row height. [Reader layout](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L132), [Message-list contract](spec/08-macos-app.md#message-list)

### Give local drafts a calm, purposeful composer state

Medium priority. A fresh local draft shows “Without a sending identity” in its top strip and From control, plus “Color Wash appearance is unavailable for this sending identity” beneath the editor. Present one useful explanation of local saving near From and reserve blocking or appearance messages for the condition they describe. Explore an optional Cc/Bcc disclosure for empty fields and separate the document, attachment and sending controls visually. Test a local-only archive, a transient catalog failure and a valid or provisional identity. Preserve From and send safety cues, keyboard access, editable draft contents and any persisted Cc/Bcc recipients. Revise the composer contract before changing the default field layout. [Composer chrome](apps/macos/Sweetbriar/Composer/ComposeWindowController.swift#L1422), [Composer contract](spec/08-macos-app.md#composer)

### Add a reader text size control

Medium priority. Add standard larger, smaller and reset actions for plain and Reading view over a bounded reader setting. Preserve relative heading scale and pitch for code and source. Validate the large size in the minimum pane and with accessibility tools. Specify separately how the controls apply to sanitized sender HTML. [Plain text](apps/macos/Sweetbriar/Reader/PlainTextReaderView.swift), [Reading renderer](apps/macos/Sweetbriar/Reader/ReadingRenderer.swift)

## Performance and qualification

### Measure reader latency by rendition and decouple optional work

High priority. The model awaits semantic extraction before displaying the body, including source, and native styling runs on the main actor. Profile short plain, long quotes, large HTML and semantic messages with the M6.4 log. Record first body, document-ready, and arrow-navigation p95 separately. If optional parsing dominates, let Sender Layout appear first when the sender has no Reading view preference, with a loading or disabled toggle until ready. Use a parked-document test, retain remembered-sender behavior and verify lease retirement on navigation. [Reader load](apps/macos/SweetbriarBridge/ReaderModel.swift#L237)

### Remeasure Search after the first-page and frame-delivery changes

High priority. Historical app p95 was 145.8 and 150.0 ms for a 100,000-message archive against 100 ms. Later M1.6r4 changes affect the result. Repeat the five query classes on the same fixture and record field, opening-page, delivery and first-frame timings, including refresh rate. If replacing the AppKit table dominates, prototype keeping the table while changing the query model. Preserve handle retirement, the specified 30 ms debounce, and selection semantics. [Recorded benchmark](plan/qualification/M1.6q2.md), [M1.6r4](plan/M1-local-archive.md#L773), [List installation](apps/macos/Sweetbriar/MessageListViewController.swift#L295)

### Profile the composer's full render after each edit

Medium priority. Draft capture compares full buffers and then updates attachments, From, safety banners and title. TextKit also styles Markdown on the main actor. Profile short drafts, long quoted replies, several attachments and marked input. Count recurring rebuilds. If chrome dominates, update only surfaces whose inputs changed. Keep byte-exact source capture, autosave behavior, undo and IME handling. [Capture path](apps/macos/Sweetbriar/Composer/ComposeWindowController.swift#L506), [Composer render](apps/macos/Sweetbriar/Composer/ComposeWindowController.swift#L1422)

### Complete the existing pilot gates

High priority. Record warm launch, cached reader, compose, filing and search percentiles on the current build. Record commit, hardware, archive size, refresh rate and sync state. Run the VoiceOver reading, reply, From choice, filing and Out Box tasks, plus the dark appearance, contrast, transparency, keyboard, RTL and Japanese input gates on synthetic data first. Follow the plan's approval and evidence requirements for real account effects and the separately scheduled power cut. [M6.4 qualification](plan/qualification/M6.4.md)

## Features and product experiments

### Jump to a mailbox or saved view by typing

Medium priority. Add a Mailbox menu command and a keyboard palette using current sidebar entries and the filing palette's behavior. Rank exact, prefix, then recent matches. Show account and full path to disambiguate duplicates. Return navigates and Escape restores focus. Preserve the four existing unified shortcuts. Test keyboard-only travel across duplicate names in two accounts and smoke-test sidebar selection. [Filing palette](apps/macos/Sweetbriar/Filing/FilingPalette.swift), [Mailbox menu](apps/macos/Sweetbriar/MainMenu.swift#L436)

### Pin favorite mailboxes and saved views

Medium priority. Prototype a short, reordered list using stable sidebar targets, with account cues on duplicates. Reuse it in the jump palette. Cover account renames, disconnected accounts, removed saved views, restoration, and keyboard reordering. Review the sidebar spec before changing its sections. [Sidebar contract](spec/08-macos-app.md#sidebar)

### Preview the outbound Markdown rendition

Medium priority. Prototype a Preview command using the core's outbound renderer for the draft and existing safe rendering controls. Include lists, code, inline attachments and quoted replies. Preserve editor selection and undo when leaving preview, block external resources, and leave draft publication and sending to their existing commands. Compare the preview against the generated outbound text and HTML for the same fixture. Only consider a side-by-side editor after validating the single-pane flow. [Composer contract](spec/08-macos-app.md#composer)

### Reply to a selected passage

Medium priority. Offer Reply with Selection for the native reader renditions. Capture message and selected text together and quote only that passage, using the existing reply target, identity checks and quote handling. Test plain and Markdown quotes, Unicode, the standalone reader window, and ordinary Reply. [Reader view](apps/macos/Sweetbriar/Reader/ReaderViewController.swift), [Composer contract](spec/08-macos-app.md#composer)

### Show a local conversation trail on demand

Exploratory. Use the existing thread window type and reader message's thread ID to show a compact read-only trail across Inbox, Sent and archive mail. Expose unread state, account and whether a link is inferred. Keep header-linked replies distinguishable from subject-only matches. Start with duplicate copies, missing parents, unrelated same-subject mail and multiple accounts. Treat a threaded message list as a separate decision. [Window types](apps/macos/SweetbriarBridge/Types.swift#L392)

### Show saved-view scope before saving

Medium priority. A scoped search saved to a view currently runs over all accounts. Show this resulting scope in the sheet when it differs from the active search. If users want persistent scope, revise the contract and retain the meaning of the older saved views. Test a query with hits in two accounts, a missing mailbox, rename, and backup or restore. [Search and saved-view contract](spec/08-macos-app.md#search-m16)

### Add a visible Save As action for attachments

Medium priority. Reuse checked attachment acquisition and copy to a destination the user chooses. Surface unavailable or damaged parts in the reader. Test cancellation, destination collisions, long Unicode names, and navigation while acquiring a file. Evaluate Quick Look separately for safe behavior across supported attachment types. [Attachment handling](apps/macos/Sweetbriar/Reader/ReaderViewController.swift#L638)

### Explain compatibility before account setup

Medium priority. Document the direct Proton workflow, local archive behavior, and supported Apple Mail and mbox import paths in the README and first-run guidance. Link backup and import help at the action. Scope and qualify general IMAP/SMTP separately if the pilot expands. [Overview](spec/00-overview.md), [Current File menu](apps/macos/Sweetbriar/MainMenu.swift#L329)

### Copy a passage as a Markdown citation

Exploratory. Add a reader context action that copies the selected passage as a Markdown block quote with its source subject and date. Let the user include the sender address when they need it. Start with native reader text selection and the standard clipboard. Support nested quotes, Unicode and multiline headers, and ensure the metadata cannot change the Markdown structure. Prototype internal links only after choosing a reference that survives restore and provides a useful missing-message state. This gives researchers and writers a quick way to collect quotations in their existing notes app.

The review fixes have separate pull requests. [Reader rendition](https://github.com/L-K-M/Sweetbriar/pull/588), [Reading view numbering](https://github.com/L-K-M/Sweetbriar/pull/593), [Account logo cache](https://github.com/L-K-M/Sweetbriar/pull/589)
