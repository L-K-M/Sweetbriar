# Eudora Mac source audit artifacts

Derived by codex (GPT-6.1-Sol) on 2026-10-11 from the Computer History
Museum's BSD-licensed release of the Eudora for Macintosh source
(mirror: https://github.com/pmachapman/eudora-mac at commit
77c41a09c29d36dd4ac18cd28e530c327746ba98).

- `menus.json`, `menu-catalog.md`: every `MENU` resource definition.
- `panels.json`, `panel-controls.md`: settings panels and controls.
- `resources.json`: extracted resource index.
- `x-eudora-settings.csv`: all 1,033 `x-eudora-setting` IDs with
  descriptions and source lines.
- `extract-resources.py`: the extraction script.

The Eudora source is "Copyright (c) 1988-2006 QUALCOMM Incorporated",
released under the BSD license by the Computer History Museum; these
derived tables retain that attribution.
