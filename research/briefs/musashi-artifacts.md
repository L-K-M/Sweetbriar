# Topic: Musashi, primary-source extraction from the original archives

Musashi is a little-known Japanese Mac email client by Sonosoft (first
released 1997; versions 1.2.4 to 3.5 for classic Mac OS and 3.4.1 for Mac OS
X). Web sources about it are thin, so we want the documentation and UI
resources inside the original distribution archives (downloaded from
https://macintoshgarden.org/apps/musashi, MD5s match the site).

`./input/` holds the BinHex-decoded forks (`<name>.data` = data fork,
`<name>.rsrc` = resource fork; decoded with `./binhex.py`):

- `mu-osx-341.sit.data`: StuffIt 5 archive, Musashi 3.4.1 for Mac OS X (PPC)
- `mu-ppc-341-en.sea.data` / `.rsrc`: StuffIt 5 self-extracting archive,
  Musashi 3.4.1 PPC, English localization
- `mu-ppc-341-jp.sea.data` / `.rsrc`: same, Japanese localization
- `MUSASHI_PPC_124J.sit.data`: Musashi 1.2.4 PPC, Japanese (StuffIt, type SITD)

No unar/deark/C compiler is available. Python 3.11 and `uv` are available
(create a venv in this directory; `rsrcfork` and `macresources` on PyPI read
resource forks).

Tasks:

1. Write a small pure-Python StuffIt 5 extractor (and classic StuffIt if
   needed for the 1.2.4 archive) that handles the compression methods these
   archives actually use (check the headers; likely method 13 "LZ+Huffman"
   and/or 15 "Arsenic"). You may consult the format documentation and The
   Unarchiver / XADMaster source on GitHub for the algorithms. Extract
   both forks of every file, preserving names (Mac Roman or Shift-JIS file
   names), type and creator codes. Keep the tool in `./tools/`.
2. Inventory what you extracted: application, read-me files, manuals,
   help files (SimpleText 'TEXT'+'styl', HTML, Apple Guide), sample files.
   Convert documents to UTF-8 text under `./extracted-docs/` (Japanese text
   is likely Shift-JIS / MacJapanese).
3. Parse the application's resource fork(s): list resource types and
   counts; dump MENU/MENU-like ('MENU', 'Mcmd', 'xmnu', 'CMNU'), 'STR#',
   'STR ', 'DITL'/'DLOG'/'ALRT' item texts, 'vers', 'BNDL'/'FREF' (document
   types it owns), 'aete' (AppleScript dictionary), and anything that
   reveals features. On Mac OS X builds, also look inside the bundle
   (Info.plist, .nib/.strings, Resources).
4. From the documents and resources, write an exhaustive feature inventory
   of Musashi (complete menu tree, preferences, filters/rules, mailbox
   model, multi-user support, encodings, POP/SMTP options, anything
   unusual), comparing versions 1.2.4 and 3.4.1 where possible. Translate
   Japanese faithfully and mark translations.
5. Explain which design ideas are distinctive and worth reviving in a
   modern macOS client (Sweetbriar), and how.

Cite the extracted file and resource (type/ID) for every claim. Do not run
or emulate the application.
