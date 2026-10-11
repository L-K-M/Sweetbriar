# Topic: Eudora for Mac 6.2.4, feature inventory from its source code

The Mac Eudora source code released by the Computer History Museum (BSD
licence) is checked out in `./src` (mirror of
https://github.com/pmachapman/eudora-mac). It is a primary source: use it
to build an exhaustive, verified inventory of what Eudora could do, and
cite file paths (and line numbers where useful) for every claim.

Questions:
1. Map the source tree: main modules, storage code (mailbox format, table
   of contents .toc files), POP/IMAP/SMTP code (note the CrispinIMAP
   directory), filters, nicknames (address book), personalities, MoodWatch,
   spell checking, stationery, plug-in/translator API (EMSAPI), AppleScript
   dictionary, help files (Eudora Help), resources (.r files: menus,
   dialogs, strings, settings).
2. Extract the complete menu structure (every menu and command) from the
   resource definitions, and the settings panels with their options. Give
   tables.
3. Filters: every match criterion and every action. Mailbox and message
   operations (Redirect, Send Again, Make Filter, Make Nickname, Transfer,
   Find Messages, labels, priorities, server status, junk scoring, etc.).
4. The "x-eudora-setting" and "Esoteric Settings": what hidden power-user
   settings existed?
5. Storage design: how messages, tables of contents, attachments and
   settings were stored on disk, and how it handled corruption (TOC
   rebuild). What can a modern client learn from it (good and bad)?
6. Performance tricks visible in the code (e.g. TOC caching, background
   tasks, connection handling on dial-up).
7. List the features that are most distinctive or that modern clients lack,
   and explain how each could be reimplemented in a modern native macOS
   client with an IMAP/JMAP/Graph backend.

Be exhaustive in the inventory; this report will be used as a reference.
