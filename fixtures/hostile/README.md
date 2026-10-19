# Hostile rendering corpus

All messages are synthetic, authored for Sweetbriar under Unlicense OR 0BSD.
No owner mail or externally copied attack corpus is included. Domains use
`.invalid`; the raw-IP link is never fetched by the tests.

`cargo test -p sb-mime --test hostile` discovers every `.eml` here. Each
message isolates a technique or spelling family. The runner fails on read,
parse, unexpected fallback, unsafe output, or an unmet header expectation.
It reparses HTML with lol_html and inline CSS with cssparser, without
sanitizing the output again. A dependency-free JSON reader checks the v1
manifest, following the existing rendition unit-test convention.

`X-Sweetbriar-Fixture` describes the attack. Optional `X-Sweetbriar-Expect`
contains space-separated assertions:

- `remote`: at least one removed network-capable resource URL was counted.
- `blocked=0`: hyperlinks and local resources do not count as remote loads.
- `resources=N`: exactly N local resources survive in the manifest.
- `missing_cid`: the renderer reports a CID outside its authorized scope.
- `link=URL|WARNING`: the sole surviving link has exactly that warning;
  `none` means no warnings. Includes mismatched mailto recipient domains.
- `fallback`: an intentional renderer fallback (no current fixture needs it).
- `dense`: replace `DENSE_BODY` with a generated 1 MiB entity-dense body,
  including ampersands and nonbreaking spaces, and check preserved text.

Rendering and body selection must finish within 30 seconds per fixture in
debug builds. The generated dense body keeps megabytes out of git. The
small files can also be passed as read-only MIME fuzz seeds; the current
MIME fuzz target exercises parsing and reader input, not the renderer.

Script spellings include every ASCII case permutation, attributes, foreign
namespaces, comments, CDATA, raw-text containers, templates and unclosed
markup. Event attributes include current, legacy, SVG and invented `on*`
names plus entity-looking names (HTML does not decode attribute names).
URL families cover all navigation/resource positions, three active schemes,
entities, case, whitespace and controls. Resource cases cover embedded
objects, hints, responsive images, media, background attributes, CSS loads
and exfiltration, forged leases, related-scope CID and MHTML/Content-Location.
Deep trees and namespace/raw-text breakouts exercise malformed HTML parsing.
