<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-12T07:11:26+00:00 exit=0 brief=research/briefs/ -->

# Proton Mail Bridge as Sweetbriar’s first live backend

**Research date: 2026-10-12 UTC.** Current facts below were checked on that date; release and issue dates are stated separately. Implementation findings use **Bridge v3.27.1**, its pinned **Gluon commit `7e800978ab4a`**, and **go-proton-api commit `8a62023d3e38`**, rather than assuming today’s development branches match the installed application. Those dependency pins come from the [tagged module manifest](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/go.mod).

This is source and documentation research, **not a live-account qualification**. No Proton credentials were used, no software was installed, and no Bridge executable or test suite was run. Statements about observed production interoperability remain explicitly qualified. Recommendations are identified as such.

The governing project decisions are the owner-provider fallback, D2’s occurrence model, and D3’s preconditioned operations, archive retention, restore quarantine, and unknown-send handling. [Project summary](../../00-summary.md), [resolution](../../debate/04-resolution.md), [backend policy](../../06-account-backends.md), [data-safety rules](../../04-data-safety.md).

## 1. TL;DR

- **Use Proton through user-installed Bridge for the first live backend.** It follows the project’s owner-provider rule and Proton’s documented desktop-client integration. Bridge requires a paid plan that includes Proton Mail. [Project resolution](../../debate/04-resolution.md), [Bridge product page](https://proton.me/mail/bridge).
- **Current public stable: 3.27.1, released 2026-09-29.** Its detailed fix concerns corrupted sync-state recovery and flushing writes. Qualify this version first and monitor subsequent releases. [Stable release notes](https://proton.me/download/bridge/stable_releases.html), [GitHub release](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.27.1).
- **Connect locally to `127.0.0.1`, normally IMAP 1143 / SMTP 1025, with STARTTLS.** Ports and TLS mode are configurable. Use Bridge’s generated password and an account-scoped certificate trust decision. [Defaults](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/vault/types_settings.go), [certificate guidance](https://proton.me/support/bridge-ssl-connection-issue).
- **Model occurrences, not independent copies.** A Proton message can appear in a folder, several `Labels/…` mailboxes, All Mail, and Starred. The Bridge-injected `X-Pm-Internal-Id` is a useful account-scoped identity signal; mailbox UID remains the mutation locator. [Label documentation](https://proton.me/support/labels-in-bridge), [header builder](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/pkg/message/build.go).
- **Bridge is a specific IMAP dialect.** It supports IDLE, UIDPLUS and MOVE, but its pinned capability set lacks CONDSTORE/QRESYNC, NAMESPACE and LITERAL+. SPECIAL-USE attributes appear in LIST despite no advertised SPECIAL-USE extension. [Capabilities](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/imap/capabilities.go), [mailbox construction](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/imap_updates.go).
- **Do not apply generic COPY → delete logic.** COPY to a real Proton folder can itself move the message; COPY to a label adds membership. MOVE to a label can preserve the source folder. [COPY cases](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/copy.feature), [MOVE cases](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/move.feature).
- **Never APPEND a routine post-send Sent copy.** Bridge saves Sent automatically. Its duplicate suppression is an implementation convenience, not durable exactly-once delivery. [Proton setup instruction](https://proton.me/support/protonmail-bridge-clients-macos-outlook-2019), [send recorder](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/sendrecorder/recorder.go).
- **BODY[] is not guaranteed original wire MIME.** Preserve exactly the bytes acquired, with Bridge provenance. Reconstruction, random boundaries and error substitutes prevent raw hashes from proving identity or complete source fidelity. [Message reconstruction](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/pkg/message/build.go), [build options](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/sync_build.go).
- **Credential-free testing is possible with real Bridge code.** Proton’s tests run Bridge against a local mock Proton API. Combine that with Dovecot and scripted faults, then qualify the official binary on the owner’s Mac. [API fixture](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/api_test.go), [Bridge fixture](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/ctx_bridge_test.go).
- **Stage delivery: read-only acquisition → read/star flags → qualified filing → SMTP.** Keep server drafts, permanent deletion and broad folder administration outside the first slice. This is a scope recommendation based on the distinct semantics and failure modes below.

## 2. Findings

### 2.1 Current Bridge, platforms, plans and operation

#### Release and platform position

On **2026-10-12**, Proton’s stable release page and GitHub’s latest-release metadata agree on **v3.27.1**, published **2026-09-29**. GitHub provides a macOS DMG, Windows installer, Linux amd64 DEB, x86_64 RPM, PKGBUILD and signatures. The public note is broad; the developer note identifies **BRIDGE-644: corrupted sync-state recovery and forced file synchronization**. [Stable channel](https://proton.me/download/bridge/stable_releases.html), [release metadata](https://api.github.com/repos/ProtonMail/proton-bridge/releases/latest).

| Platform | Verified distribution/support position on 2026-10-12 | Planning implication |
|---|---|---|
| macOS | Official application; Apple silicon supported. Proton’s requirements page currently lists **macOS 15 and 26**, not 27. | macOS 27 is the project target, but official support/qualification for it is **(unverified)**. |
| Linux | Official 64-bit DEB, RPM and PKGBUILD; latest Ubuntu LTS and latest Fedora Workstation supported. | Use Proton’s official package for initial Linux qualification. |
| Linux ARM | Requirements exclude ARM except Apple silicon Macs. | Do not promise an official Linux ARM binary. |
| Flatpak | Flathub supplies `ch.protonmail.protonmail-bridge`, explicitly described as a community package unsupported by Proton. | Treat as another packaging environment, not an official Proton distribution. |
| Windows | Official 64-bit Windows 10 22H2 and Windows 11 support. | Available for later transport testing; outside the first UI target. |

Sources: [Proton requirements](https://proton.me/support/operating-systems-supported-bridge), [Linux packages](https://proton.me/support/bridge-for-linux), [Flathub package](https://flathub.org/en/apps/ch.protonmail.protonmail-bridge).

The **2026-08-17 v3.26.0** notes include a macOS 27 crash fix. That establishes active compatibility work; it does not establish that Proton has updated its formal support matrix. [v3.26.0 release](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.26.0).

Bridge is available only with a **paid plan including Proton Mail**; a Free account is insufficient. Avoid describing every paid Proton product as eligible: the Bridge product page explicitly conditions eligibility on including Mail. No additional Sweetbriar-specific fee or registration requirement was found in the consulted integration documentation. [Product eligibility](https://proton.me/mail/bridge), [IMAP/SMTP introduction](https://proton.me/support/imap-smtp-and-pop3-setup).

#### How the user starts it

The documented sequence is: install Bridge, open it, sign in inside Bridge, then copy its mailbox configuration into the email client. The default address mode is combined. Closing the main window leaves Bridge running; quitting Bridge ends client connectivity. [Installation guide](https://proton.me/support/protonmail-bridge-install), [tagged README](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/README.md).

Bridge also has **Open on startup**, enabled by default. Proton’s macOS troubleshooting describes `~/Library/LaunchAgents/Proton Mail Bridge.plist`, so startup is an existing Bridge-managed user-session facility. Sweetbriar does not need its own mail agent to make Bridge run. [Autostart documentation](https://proton.me/support/automatically-start-bridge).

Two headless modes exist in v3.27.1:

| Invocation | Behavior |
|---|---|
| `--cli` / `-c` | Interactive terminal interface for account/configuration operations. |
| `--noninteractive` / `-n` | Runs without an interactive frontend; requires previously configured accounts and accessible key storage. |

These are distinct alternatives in [frontend dispatch](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/app/frontend.go). The [build guide](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/BUILDS.md) documents both and `make build-nogui`; its older Go prerequisite text should not override the current module manifest.

Documented CLI launch examples, with an ordinary ASCII hyphen:

```sh
# macOS
"/Applications/Proton Mail Bridge.app/Contents/MacOS/Proton Mail Bridge" --cli

# Linux
protonmail-bridge --cli
```

The executable paths and `-c` are documented by [Proton’s CLI guide](https://proton.me/support/bridge-cli-guide). The exact noninteractive behavior of an official installed launcher should be checked with that installation’s `--help`; the source verifies the underlying mode.

Linux requires supported secret storage, such as Secret Service/GNOME Keyring or `pass`. Headless execution does not remove that requirement. [Linux documentation](https://proton.me/support/bridge-for-linux).

**Recommendation:** onboarding should guide installation, open Bridge, explain how to copy credentials, and probe the configured local endpoint. Support ordinary GUI/autostart operation first. Do not silently reset Bridge or manipulate its private vault to “repair” connectivity.

### 2.2 Endpoints and security

#### Connection contract

The listener binds **`127.0.0.1`**. Default settings start port selection at **1143 for IMAP** and **1025 for SMTP**, choosing available ports during initialization; both default SSL booleans are false, corresponding to STARTTLS rather than implicit TLS. Existing settings can differ. [Host constant](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/constants/constants.go), [listener](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapsmtpserver/listener.go), [settings defaults](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/vault/types_settings.go).

| Setting | Sweetbriar handling recommended |
|---|---|
| Host | Use literal `127.0.0.1`; restrict this preset to loopback. |
| IMAP/SMTP port | Prefill 1143/1025; allow explicit values copied from Bridge. |
| STARTTLS | Require the upgrade before credentials or mail content; refresh capabilities afterward. |
| “SSL” in Bridge | Interpret as TLS from connection establishment; use the displayed port, not an assumed 993/465. |
| Credentials | Store the Bridge-generated password and displayed username, not the Proton account password. |

Proton documents configurable ports and STARTTLS/SSL selection. Its port guide specifically warns that the defaults may already be occupied. [Settings guide](https://proton.me/support/comprehensive-guide-to-bridge-settings), [port troubleshooting](https://proton.me/support/port-already-occupied-error).

The SMTP server permits insecure authentication in source, supporting compatibility workarounds. That is **not** a reason for Sweetbriar’s preset to downgrade automatically: TLS can be mandatory in the client even when optional in the server. [SMTP server construction](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapsmtpserver/smtp.go).

#### Certificate trust and export

Bridge generates a self-signed TLS certificate for its local servers. Proton’s intended client flow is accepting that certificate or importing it; normal public-CA validation alone will fail. [Certificate explanation](https://proton.me/support/macos-certificate-warning).

The tagged certificate template includes **IP SAN `127.0.0.1`** and common name `127.0.0.1`; it does not establish a `localhost` DNS SAN. This supports using the literal IP consistently. [Certificate template](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/certs/tls.go).

**Settings → Advanced → Export TLS certificates** exports the certificate and private key. The implementation writes **`cert.pem` and `key.pem`**, with mode `0600`. Sweetbriar needs only the public certificate. [Export UI documentation](https://proton.me/support/comprehensive-guide-to-bridge-settings), [export implementation](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/frontend/grpc/service_cert.go).

**Recommended trust design:** import `cert.pem` into a connection-specific trust configuration and retain its SHA-256 DER fingerprint. Validate the TLS handshake and certificate for the configured endpoint; check that the leaf matches the approved certificate. Scope approval to this Bridge installation and its IMAP/SMTP endpoints. Show a repairable connection error on certificate change, requiring a renewed trust decision. Do not add an unrestricted system trust root or implement an accept-all verifier.

A first-use fingerprint prompt is an alternative, but importing Bridge’s exported certificate gives the user a separate source for comparison. Neither protects against a fully compromised local machine; this is a local-server authentication decision. Proton’s own server-side pinning concerns **Bridge → Proton**, a different connection. [Bridge security model](https://proton.me/blog/bridge-security-model).

#### Passwords and address modes

The email client uses a **unique Bridge password different from the Proton login password**. The user enters Proton credentials into Bridge itself. Sweetbriar should never request Proton’s mailbox decryption password or recovery phrase. [Product security description](https://proton.me/mail/bridge), [client credential instructions](https://proton.me/support/protonmail-bridge-clients-windows-outlook-2019).

Combined mode places all account addresses in shared mailbox views; split mode exposes address-specific views configured separately. Additional sending addresses still need client identity configuration. Changing modes requires client reconfiguration in Proton’s instructions. [Address-mode documentation](https://proton.me/support/difference-combined-addresses-mode-split-addresses-mode), [additional-address guide](https://proton.me/support/mail-bridge-addresses).

**Recommendation:** one Proton account in combined mode first, with explicit From identities. Persist the selected address mode and connection incarnation. A mode change must trigger inventory reconciliation and suspend queued writes; it must not create several unrelated local accounts for the same Proton mailbox.

### 2.3 IMAP behavior and quirks

#### Capabilities: inspect the authenticated connection

The pinned Gluon tests distinguish pre-authentication and authenticated capability sets. Authentication, STARTTLS state and remote feature flags affect availability. In particular, IDLE can be disabled by a kill switch, and Bridge can disable IMAP AUTHENTICATE while retaining LOGIN. [Session initialization](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/internal/session/session.go), [capability transcript tests](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/tests/capability_test.go).

| Capability | v3.27.1 source position | Engine consequence |
|---|---|---|
| IMAP4rev1 | Present | Implement rev1 behavior, not assumed rev2. |
| IDLE | Present unless disabled | Use for notification, with polling fallback. |
| UIDPLUS | Present after authentication | APPENDUID, COPYUID and targeted UID EXPUNGE are available protocol mechanisms. |
| MOVE | Present after authentication | Still qualify Proton’s semantic behavior. |
| UNSELECT | Present after authentication | Leave a mailbox without CLOSE’s implicit expunge. |
| ID | Present | Identify Sweetbriar honestly; do not impersonate another client. |
| AUTH=PLAIN / STARTTLS | Conditional on configuration and connection state | Negotiate TLS first and discover current auth methods. |
| CONDSTORE / QRESYNC | Absent | No HIGHESTMODSEQ/UNCHANGEDSINCE/VANISHED-based sync assumption. |
| NAMESPACE / LITERAL+ | Absent | Discover hierarchy through LIST; use synchronized APPEND literals. |
| SPECIAL-USE | Not advertised | Nevertheless inspect LIST attributes, which Bridge supplies. |
| SORT / THREAD / Gmail extensions | Not in the pinned capability set | Sort/thread locally; do not use X-GM-MSGID or X-GM-LABELS by assumption. |

Sources: [capability constants](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/imap/capabilities.go), [system mailbox attributes](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/imap_updates.go). UIDPLUS mechanisms are defined in [RFC 4315](https://www.rfc-editor.org/rfc/rfc4315).

An X-GM-EXT-1 proposal exists upstream; a proposal is not evidence that stable Bridge implements it. Similarly, Gluon issue **#378**, opened **2023-07-05**, remains open requesting SORT/THREAD support. [Gmail-extension PR](https://github.com/ProtonMail/gluon/pull/450), [SORT/THREAD issue](https://github.com/ProtonMail/gluon/issues/378).

#### Mailbox mapping and identity

Expect system mailboxes **INBOX, Drafts, Sent, Starred, Archive, Spam, Trash, All Mail**, plus **`Folders/<path>`** and **`Labels/<name>`**. Parent `Folders` and `Labels` entries are `\Noselect`; the delimiter is `/` in the supplied tests. Scheduled is additionally supported and hidden when empty. All Mail can be hidden by Bridge settings. [LIST cases](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/mailbox/list.feature), [mailbox mapping](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/helpers.go).

Proton’s regular folders are generally exclusive; labels add multiple memberships, and Starred/All Mail are aggregate views. **All Mail coverage is configurable**: Proton’s web settings can exclude Spam and Trash. Therefore summing folder and label counts overcounts messages, while relying on All Mail alone can miss excluded locations. [Folder/label documentation](https://proton.me/support/using-folders-labels), [Bridge labels](https://proton.me/support/labels-in-bridge).

The build options add `X-Pm-Internal-Id`, `X-Pm-External-Id`, `X-Pm-Date`, and a Proton reference. The builder sets the internal header from the Proton message ID rather than accepting an arbitrary sender-supplied value. Gluon separately uses `X-Pm-Gluon-Id`, an internal local identifier. [Build options](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/sync_build.go), [header assignment](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/pkg/message/build.go), [Gluon header key](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/internal/ids/header.go).

**Recommended D2 mapping:** retain local logical-message UUIDs and a distinct occurrence for every `(account, connection incarnation, mailbox, UIDVALIDITY, UID)`. Use `(Proton account, X-Pm-Internal-Id)` as a qualified provider identity to associate appearances, preserving the evidence used. Do not merge by RFC Message-ID, matching subject or matching bytes. Do not use the Gluon header as a permanent provider identity.

The internal header’s persistence across every account removal, import and future release is **(unverified)** as a public contract. If it is absent, contradictory or reused with incompatible content, keep separate records and quarantine correspondence. Identity association does not authorize a queued destructive intent to jump to a new occurrence.

#### UIDVALIDITY and rebuilding

Gluon generates mailbox UIDVALIDITY using elapsed seconds since **2023-02-01**, incrementing within a process to avoid same-second collisions. It is assigned during mailbox creation, not changed on every FETCH. Stored mailbox state is the basis for ordinary reconnects. [UIDVALIDITY generator](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/imap/uid_validity_generator.go), [mailbox creation](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/internal/backend/connector_updates.go).

Bridge’s refresh/resync and address-mode-change paths remove IMAP connectors with data deletion and recreate them. Deleted Gluon files also trigger resynchronization in the integration suite. Thus a full reconstruction should be treated as an epoch-changing event; a simple connection loss should not be assumed to reset identity. Ordinary logout/login and account removal are distinct paths, and their exact UID preservation on the official binary remains a manual gate. [Refresh/mode lifecycle](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/service.go), [restart/cache-loss tests](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/user/sync.feature), [relogin tests](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/user/relogin.feature).

The snapshot’s warning about UIDs changing without UIDVALIDITY has historical primary evidence: archived **Bridge #220**, opened **2021-09-29**, reports Bridge **1.8.9**. A Proton maintainer’s **2022-11-09** response attributes fixes to the forthcoming v3 Gluon snapshot/response-order design. This does **not** establish that 3.27.1 currently has the same bug. [Archived issue and maintainer discussion](https://web.archive.org/web/20221203013158/https://github.com/ProtonMail/proton-bridge/issues/220).

**Recommendation:** recheck epochs on every mailbox selection; suspend dependent intents on an epoch change. Before a mutation, revalidate UID → provider-header correspondence. On an unexplained changed mapping, close/rebuild the inventory and pause writes. A differing raw content hash alone is insufficient because Bridge reconstructs MIME.

#### Flags and keywords

The basic permanent flags in the connector are **`\Seen`, `\Flagged`, `\Deleted`, `$Forwarded`, `Forwarded`**, with All Mail restricted to Seen/Flagged in system-mailbox construction. Seen calls Proton read/unread operations; Flagged applies/removes Starred. Metadata also produces `\Draft` and `\Answered`. [Connector defaults and flag methods](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/connector.go), [metadata flags](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/helpers.go).

Gluon treats `\Deleted` as a **mailbox-membership flag**, while normal read/star changes operate on the underlying message. Arbitrary client tags are not Proton labels and are not synchronized as labels; do not assume a writable `\*` keyword namespace. Direct STORE/readback of Answered appears in tests, but cross-device persistence is a separate question. [Flag state implementation](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/internal/state/updates.go), [STORE cases](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/store.feature), [Proton tag limitation](https://proton.me/support/labels-in-bridge).

**Recommendation:** qualify read/unread and star/unstar first; keep Sweetbriar tags and notes local. Preserve flags per occurrence as D2 requires, even when provider semantics cause several occurrences to converge. Without CONDSTORE, flag updates have a check/write race: use add/remove deltas, serialize local intents and reconcile external changes rather than claiming conditional atomicity.

#### COPY, MOVE and EXPUNGE

This table summarizes tagged source/tests; it is a qualification specification, not a record of commands executed against production.

| Operation | Source-defined behavior | Sweetbriar rule recommended |
|---|---|---|
| COPY into `Labels/L` | Adds label membership; keeps original folder. | Use for Add label. |
| COPY into `Folders/F` | Can remove prior folder membership: effectively filing. | Treat COPY as potentially mutating the source. |
| MOVE folder → label | Applies label while retaining folder membership. | Do not issue a second source deletion. |
| MOVE label → label | Adds destination label, removes source label. | Explicit Replace label only. |
| MOVE label → folder | Files into destination and removes source label. | Not a generic “remove label” implementation. |
| COPY All Mail → folder | Files from the original folder. | Do not regard aggregate-view COPY as nondestructive. |
| MOVE from/to All Mail; APPEND to All Mail | Disallowed. | Resolve a qualified real occurrence or defer. |
| MOVE Inbox ↔ Sent | Disallowed. | No general filing to these destinations. |
| COPY Inbox → Sent | Test expects success but no Sent membership. | Tagged OK alone does not prove the requested effect. |
| EXPUNGE label membership | Unlabels the underlying message. | No generic delete/purge from label mailboxes. |
| EXPUNGE ordinary folder | Membership handling depends on folder-unlabel behavior. | Do not synthesize Trash or archive with EXPUNGE. |
| EXPUNGE Trash/Drafts | Can permanently delete if no other qualifying memberships remain. | Defer permanent deletion and server draft replacement. |
| EXPUNGE All Mail | Rejected in tests. | Never use for account-wide deletion. |

Sources: [COPY cases](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/copy.feature), [MOVE cases](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/move.feature), [ordinary deletion cases](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/delete.feature), [Trash cases](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/delete_from_trash.feature).

Two further traps matter. First, the MOVE tests explicitly document that **APPEND + delete + EXPUNGE into a label does not preserve the original folder**, unlike the native MOVE-to-label path. Second, **v3.24.1, 2026-04-08**, restored old folder-unlabel logic under a feature flag; both branches remain in v3.27.1. The release number alone does not determine every filing behavior. [MOVE limitation](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/move.feature), [v3.24.1 release](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.24.1), [connector branches](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/connector.go).

**Recommendation:** initially forbid all client-issued EXPUNGE in the Proton preset, including UID EXPUNGE, and never use CLOSE to leave a deleted-marked mailbox. Use qualified UID MOVE for filing and COPY-to-label for label addition. Defer pure Remove label; later it could explicitly authorize targeted UID EXPUNGE as membership removal after dedicated tests. A permanent ban on every label expunge would unnecessarily prevent that operation. Generic delete and label removal must nevertheless remain different commands. [Project IMAP rules](../../04-data-safety.md), [MOVE protocol](https://www.rfc-editor.org/rfc/rfc6851).

#### APPEND, size and MIME fidelity

APPEND to **Sent** is an import operation, not an instruction to submit mail. APPEND to **Drafts** is supported, but editing/replacement and subsequent deletion have additional semantics. Bridge may match a subsequent Sent APPEND against its recent-send recorder. [Creation cases](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/create.feature), [send-plus-APPEND case](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/smtp/send/send_append.feature).

Failed imports may be retained in **Recovered Messages**, a device-local Bridge mailbox lost on Bridge reset or account removal. Proton explicitly describes clients deleting the source despite a failed APPEND. An oversized import is specially remapped in current source to avoid this recovery path, so the support article’s “any failed APPEND” wording should not be treated as an unconditional implementation guarantee. [Recovery documentation](https://proton.me/support/what-is-the-recovered-messages-folder-in-bridge), [import handling](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/connector.go).

Current size facts should remain separate:

| Constraint | Verified fact on 2026-10-12 |
|---|---|
| Incoming attachments | Proton documents **50 MB total**. |
| Outgoing attachments | Proton documents **25 MB total**, at most **100 files**; encoding increases size. |
| Bridge import release note | v3.22.0 notes say import limit increased to **55 MB**. |
| Pinned API import implementation | Checks an **encrypted import representation** against **70 × 1024 × 1024 bytes**; its error string still says 50 MB. This is not a raw IMAP literal guarantee. |

Sources: [attachment limits](https://proton.me/support/attaching-multiple-documents-to-a-message), [tagged changelog](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/Changelog.md), [API import code](https://raw.githubusercontent.com/ProtonMail/go-proton-api/8a62023d3e38/message_import.go).

**Recommendation:** stream and bound fetched literals; do not reject incoming reconstructed MIME merely because its encoded RFC822 size exceeds the outgoing attachment limit. Qualify actual upload boundaries with expendable test messages. Do not turn the three different import numbers into one promised universal limit.

Bridge builds RFC822 from decrypted Proton metadata, body and attachments. Header sanitation, transfer encoding and MIME assembly mean **sender-original wire bytes are not guaranteed**. Since **v3.25.0, 2026-06-01**, MIME boundary generation is stochastic; the source uses 32 random bytes represented as 64 hexadecimal characters. [v3.25.0 release](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.25.0), [MIME builder](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/pkg/message/build.go).

The PGP-MIME path can extract signatures and construct multipart/signed output, or retain an encrypted fallback when decryption fails. Tests include signature verification. This does not prove byte-exact preservation for every DKIM, PGP or S/MIME message, nor expose a general Proton signature-verification status API through IMAP. [PGP reconstruction tests](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/pkg/message/build_test.go), [decryption](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/pkg/message/decrypt.go).

Another coverage trap: initial-sync building can record failed message IDs and omit a result; event processing also has a synthetic **“Message failed to build”** literal path. A successfully fetched error substitute is not complete mail. Do not classify one solely by subject, which real mail can share. [Initial build stage](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/syncservice/stage_build.go), [event processing](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/service_message_events.go), [substitute construction](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/sync_build.go).

**Recommendation:** D1 should preserve **byte-exact acquired literals**, tagged `source=proton_bridge`, acquisition version/date and occurrence. Preserve later differing reconstructions as new acquired variants when necessary. Separate object integrity hashes from correspondence heuristics and coverage. A Bridge export must not claim cryptographically authenticated sender-original MIME.

#### Relevant defect inventory

An open issue is evidence of a report, not proof that every current installation reproduces it. Status below was checked **2026-10-12**.

| Evidence | Date/version | Status and relevance |
|---|---|---|
| [Gluon #498](https://github.com/ProtonMail/gluon/issues/498) | Opened **2026-10-06**, Bridge 3.27.0 | Open report: raw UTF-8 in ENVELOPE quoted strings broke a strict Rust parser and desynchronized the response stream. Prefer header literals for metadata; discard a broken connection safely. Reproduction on 3.27.1 is **(unverified)**. |
| [Gluon #482](https://github.com/ProtonMail/gluon/issues/482) | **2026-08-10**, Bridge 3.25.0 | Open report: empty undisclosed-recipient group parsing fails for a signed message. Include recipient-group/S/MIME cases. |
| [Gluon #452](https://github.com/ProtonMail/gluon/issues/452) | **2026-02-25**, Bridge 3.22.0 | Open report: resync leaves orphaned databases/stores. Budget disk headroom and observe repair; do not adopt the reporter’s filesystem-limit speculation as a universal APFS fact. |
| [Gluon #471](https://github.com/ProtonMail/gluon/issues/471) | **2026-05-21**, Bridge 3.24.2 | Open interoperability report: arbitrary top-level CREATE breaks Mailspring state folders. Create only within Bridge’s Folders/Labels model. |
| [Gluon #386](https://github.com/ProtonMail/gluon/issues/386) | **2023-08-05** | Open older report: import validation rejects previously received mail. Treat as a fixture case; current reproduction is **(unverified)**. |
| [Bridge 3.27.0](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.27.0) | **2026-09-09** | Recent fixes: BODYSTRUCTURE response compliance, remote-message-ID SQL table, deletion chunking; maximum MIME depth added. |
| [Bridge 3.27.1](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.27.1) | **2026-09-29** | Sync-state corruption recovery/flush fix. Use as baseline rather than an older release. |

The repository’s [current metadata](https://api.github.com/repos/ProtonMail/proton-bridge) has `has_issues=false`; the old Bridge #220 URL is unavailable live. Monitor public release notes and Gluon issues, and preserve archived primary evidence. The reason or completeness of historical issue removal is **(unverified)**.

### 2.4 SMTP submission, Sent and failures

Bridge’s SMTP server offers SASL **PLAIN and LOGIN**. Submission parses the client MIME, creates a Proton draft, uploads/encrypts attachments, obtains recipient preferences and calls `SendDraft`. This is not raw SMTP relay passthrough to an Internet mail server. [Server authentication](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapsmtpserver/smtp.go), [submission implementation](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/smtp/smtp.go).

Proton explicitly instructs clients to disable saving their own Sent copies because Bridge saves them automatically. The feature suite verifies a Sent occurrence after SMTP alone and duplicate avoidance for a matching subsequent APPEND. **Sweetbriar must not APPEND a routine post-send Sent copy**, even if the synced Sent copy is temporarily delayed. [Outlook setup instruction](https://proton.me/support/protonmail-bridge-clients-macos-outlook-2019), [Sent feature](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/smtp/send/send_append.feature).

Sending addresses must exist on the Proton account. Source rejects disabled/non-sendable addresses, checks ownership, and can prefer the MIME **From** address over SMTP MAIL FROM. Plus-address handling is implemented. Recommendation: qualify SimpleLogin/hide-my-email workflows separately rather than assuming every alias is an account sending identity. [Address documentation](https://proton.me/support/mail-bridge-addresses), [sender handling](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/smtp/smtp.go).

**Recommendation:** freeze authentication identity, From, envelope sender, recipients and MIME in the durable send attempt; keep From and envelope sender consistent. A change of UI identity after queueing must not reroute an existing attempt.

RCPT handling accumulates recipients without verifying all eventual Proton delivery conditions. Some errors therefore arrive at DATA completion, not individual RCPT. Preserve the complete SMTP transcript/recipient acceptance state, but never represent a 250 RCPT as delivery. [SMTP session](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/smtp/smtp_backend.go).

Bridge maps failures to human-readable errors for invalid recipients, unsupported content, invalid sender and excessive failures. The pinned SMTP library converts an ordinary DATA error into **554**, while success becomes **250 queued**. A transient upstream failure can therefore arrive through a generic negative result without a useful retry classification. [Error mapper](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/smtp/errormapper.go), [wire status mapping](https://raw.githubusercontent.com/ProtonMail/go-smtp/2b3d50599865/conn.go).

The server does not set a numeric `MaxMessageBytes` in its construction; the pinned library advertises **SIZE without a numeric limit** in that case. EHLO is not sufficient to discover Proton’s 25 MB attachment policy. Check that policy locally and preserve server rejection details. [SMTP construction](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapsmtpserver/smtp.go), [SIZE advertisement](https://raw.githubusercontent.com/ProtonMail/go-smtp/2b3d50599865/conn.go).

The send recorder stores hash/recipient entries **in memory**, with a configured **30-minute expiry**. Submission and matching APPEND use a **90-second wait deadline** for pending entries; that is **not** the duplicate-retention duration. Neither survives arbitrary Bridge restarts as a durable idempotency contract. [Recorder implementation](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/sendrecorder/recorder.go).

**Recommendation:** final positive SMTP completion means accepted, not delivered. If the connection drops after DATA may have been accepted, persist **Outcome unknown** and do not resend automatically. Inspect Sent for supporting evidence, but Message-ID/content matching is not conclusive proof of non-delivery. Keep exact submitted MIME locally even though the eventual Sent reconstruction can differ. Missing Sent filing never causes resubmission. [Project send rules](../../04-data-safety.md).

### 2.5 Synchronization, performance and lifecycle

There are **two synchronization stages**: Proton → Bridge, then Bridge → Sweetbriar. Bridge stores an encrypted local message cache and Gluon database; Sweetbriar’s archive, index and backups are separate copies with their own protection requirements. A warm Bridge can still feed a cold Sweetbriar, and vice versa. [Cache documentation](https://proton.me/support/comprehensive-guide-to-bridge-settings), [store construction](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapsmtpserver/imap.go).

No defensible universal initial-sync duration was found. Current source uses a staged metadata/download/build/apply pipeline, memory budgets and bounded parallel downloads; the settings default is **2 GiB maximum sync-memory configuration**, not a measured promise about peak RSS. The dependency and machine workload determine actual time. [Sync limits](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/syncservice/limits.go), [memory setting](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/vault/types_settings.go).

**Recommendation:** measure time to first useful inbox, Bridge-complete inventory, Sweetbriar headers-complete inventory and bodies-complete archive separately. Record account message count/bytes, CPU, disk, Bridge version, TLS mode and warm/cold state. Begin with one IDLE connection and one worker connection, bounded UID batches and interactive fetch priority; increase concurrency only from measurements.

The production defaults for upstream event polling are **20 seconds plus up to 20 seconds jitter**. Gluon’s default IDLE bulk interval is **500 ms**. These are implementation timing inputs, not a guaranteed end-to-end arrival latency; event processing, downloads, offline state and disabled IDLE add delay. [User event constants](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/user/user.go), [jitter definition](https://raw.githubusercontent.com/ProtonMail/go-proton-api/8a62023d3e38/ticker.go), [Gluon builder](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/builder.go).

Proton’s delayed-mail guide recommends more frequent client polling, including one minute. **Recommended initial fallback:** poll active/inbox views about every 60 seconds while awake, with a less frequent complete inventory/flag sweep and exponential reconnect backoff. Renew IDLE within the RFC’s advised 29-minute interval, and treat notifications as invalidations to reconcile rather than a complete journal. [Delay guidance](https://proton.me/support/how-to-troubleshoot-messages-received-with-a-delay-in-your-email-client), [RFC 2177](https://www.rfc-editor.org/rfc/rfc2177).

No published stable **Bridge requests-per-minute quota** was verified. The API dependency handles HTTP 429/503 and Retry-After with jitter; the client sees Bridge’s effects rather than raw API throttling headers. SMTP also has a rapid-failure counter threshold of three and a default **20-second cooldown**. These do not define a general sending allowance. [API retry handling](https://raw.githubusercontent.com/ProtonMail/go-proton-api/8a62023d3e38/response.go), [SMTP failure limiter](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/smtp/accounts.go).

During initial sync, mailboxes can be partially populated. Public historical notes describe allowing outbound operations during sync, while current source has a distinct synchronization event handler and rewinds the upstream event cursor after completing backfill. Do not extrapolate older wording into a guarantee that every event is handled identically today. [Stable synchronization history](https://proton.me/download/bridge/stable_releases.html), [current event handling](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/service.go).

**A completed IMAP inventory proves only Bridge-visible inventory at that time.** IMAP does not provide this client with Bridge’s internal failed-message list or a portable “Proton backfill complete” signal. Build failures can omit mail; hidden All Mail and web exclusion settings change visible coverage. Display “Bridge-visible scan complete” separately from claims about all Proton mail. During the pilot, the owner should confirm Bridge’s progress/error state and reconcile inventories with Proton’s web/export views.

Repair Bridge reloads accounts, refreshes cache and downloads mail; Proton explicitly says messages may temporarily disappear from the email client. Restarting normally preserves synchronized state in tests, but repair/cache loss/mode changes rebuild it. Updates and upstream connectivity failures can disconnect both local servers. [Repair settings](https://proton.me/support/comprehensive-guide-to-bridge-settings), [persistence tests](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/user/sync.feature), [connection behavior](https://proton.me/support/how-to-resolve-connection-issues-in-bridge).

**Recommendation:** classify refused connection, signed-out account, certificate change, changing epoch, partial coverage and healthy idle state separately. Preserve the previous complete inventory during rebuild; new empty/partial views must not purge local originals, replay stale filing, or trigger bulk deletion. Pause write/recovery-GC on unexplained mass disappearance as D3 requires. Offline reading and local drafting remain available without Bridge.

### 2.6 Terms, policy and licensing

**Checked 2026-10-12:** Proton’s terms show revision **2026-10-09**. The **2026-10-03** archived document shows **2026-06-23**. The October revision adds general modify/suspend/discontinue language in section 4; section 2’s restriction on automation distinguishable from normal human-client behavior was already present. This is consistent with the project fact check; no new Bridge-specific ban was verified. [Current terms](https://proton.me/legal/terms), [archived terms](https://web.archive.org/web/20261003105539id_/https://proton.me/legal/terms), [project audit](../../raw/fact-check-1/codex.md).

Bridge’s documented purpose is third-party desktop IMAP/SMTP access. **Recommendation/inference:** ordinary local Sweetbriar operation through user-installed Bridge is the authorized product path described by Proton, unlike implementing an unofficial service API. The consulted documentation does not promise Sweetbriar certification or permission for abusive automation. Use normal client identification, bounded traffic and user-owned accounts; no automated account farming. [Official integration purpose](https://proton.me/support/imap-smtp-and-pop3-setup), [terms](https://proton.me/legal/terms).

Proton separately offers business SMTP submission, which is send-only and has different encryption semantics. It is not the incoming backend discussed here. [Business SMTP documentation](https://proton.me/support/smtp-submission).

Bridge’s copying notes specify **GPLv3 or later**. Using its separate process over IMAP/SMTP is architecturally different from copying/linking Bridge implementation into Sweetbriar. The FSF’s guidance distinguishes independent aggregation from combined programs according to the actual relationship. **Recommendation:** keep Bridge user-installed and independent; do not bundle or embed it for the pilot. Keep reused Bridge test code in a separately licensed CI component and assess redistribution obligations if shipping that component. This is a distribution recommendation, not a definitive licence opinion about a hypothetical bundle. [Bridge copying notes](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/COPYING_NOTES.md), [FSF aggregation guidance](https://www.gnu.org/licenses/gpl-faq.html#MereAggregation).

### 2.7 Testing without real credentials, and manual qualification

#### A real credential-free route exists

The claim “there is no fake Bridge” needs qualification. Proton does not supply a stable, ready-made fake IMAP appliance, but it supplies a **fake Proton API underneath the actual Bridge core**. `tests/api_test.go` defaults to `go-proton-api/server.New()`; setting `FEATURE_TEST_HOST_URL` selects a live proxy instead. `tests/ctx_bridge_test.go` constructs Bridge with test keychains and mocked lifecycle dependencies. These tests exercise real local IMAP/SMTP behavior without real Proton accounts. [API fixture](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/api_test.go), [core fixture](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/ctx_bridge_test.go).

The suite uses `godog`, runs under `TestFeatures`, and exposes `FEATURES` for selected cases. `make test-integration` uses the `test_integration` build tag. Real Gmail scenarios are excluded by the default tag selection. **However, default selection also excludes `@regression`**, so an ordinary successful run does not cover every named regression. [Testing guide](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/README.md), [test target](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/Makefile), [tag selection](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/bdd_test.go).

A source-derived starting command, **not executed in this research**, is:

```sh
# Run inside the pinned Bridge source directory after build prerequisites.
# Leave FEATURE_TEST_HOST_URL unset so the fake API is selected.
env -u FEATURE_TEST_HOST_URL \
  FEATURES="$PWD/tests/features/imap/message/copy.feature" \
  go test -tags=test_integration -run '^TestFeatures$' -count=1 ./tests
```

This runs Proton’s tests, not Sweetbriar’s client. To qualify Sweetbriar, add a small CI-only fixture driver inside the pinned Bridge module: provision mock users/messages, start the core, report the selected ports and test certificate, and let the Rust engine connect through its normal protocol boundary. Pin its dependency graph and disable live network access after dependency preparation. Adapting the harness is implementation work still to be done; no ready-to-run Sweetbriar fixture is claimed here.

The tagged module specifies **Go 1.26.1 with toolchain 1.26.7**. Its older build guide still says Go 1.24.0. Use the actual pinned manifest when preparing CI; expect native/CGO prerequisites, but the test harness need not build the Qt GUI. [Module manifest](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/go.mod), [build guide](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/BUILDS.md).

#### Recommended CI layers

| Layer | Purpose | Required cases |
|---|---|---|
| Rust engine/store simulation | Durable intent and recovery invariants. | Crash at every receipt boundary, disk full, stale epoch, restored pending send, partial inventory. |
| Dovecot | Correct generic IMAP behavior. | UIDPLUS, MOVE, IDLE, flags, APPEND, mailbox recreation and UIDVALIDITY changes. |
| Scripted server or fault proxy | Deliberate failures Dovecot will not naturally produce. | Same-epoch UID remap, malformed ENVELOPE, interleaved unsolicited replies, lost tagged completion, silent IDLE, changing certificate. |
| Pinned real Bridge + mock Proton API | Proton dialect contract. | COPY-as-filing, label membership, All Mail restrictions, folder-unlabel branches, reconstruction, failed imports, automatic Sent. |
| Owner’s official Bridge/Mac | Real packaging/service/OS evidence. | Startup, TLS, credential lifecycle, production account inventories, throttling and send outcomes. |

Dovecot’s `virtual` plugin can expose query-defined aggregate mailboxes, which is useful for multi-view fixtures. It does **not** make ordinary Dovecot label/folder mutations identical to Proton’s. A scripted façade needs to enforce those semantic branches or the test should use actual Bridge. Dovecot `imaptest` is an additional protocol test tool, not a substitute for testing Sweetbriar’s engine. [Virtual plugin](https://doc.dovecot.org/2.4.5/core/plugins/virtual.html), [imaptest](https://dovecot.github.io/imaptest/).

For every mutation test, capture commands and fail on unauthorized APPEND, bare EXPUNGE, CLOSE after deletion marking, or automatic retransmission of an unknown send. Check the provider state behind the fixture, not merely a client-success response. Force the folder-unlabel feature flag both ways. Test missing headers, malicious duplicate identity headers and MIME boundary changes without inventing message merges.

#### What must be qualified on the owner’s Mac

Use the official **3.27.1** application, record the actual macOS 27 build and architecture, and keep a dated qualification artifact. Start with read-only observations of the real account. For mutation/send tests, use clearly marked expendable messages and controlled recipients, with a verified backup and retained local originals.

1. **Setup and lifecycle:** fresh installation; GUI startup; Open on startup; login/Keychain availability; Sweetbriar starting before Bridge; Bridge quit/restart; Mac sleep/wake; network loss; port conflict; official update. Confirm offline reading survives all of them.
2. **TLS:** record actual IMAP/SMTP ports, modes and certificate fingerprints; import `cert.pem`; test required STARTTLS and implicit TLS; verify certificate replacement produces a recoverable trust error.
3. **Protocol:** capture greeting/CAPABILITY before and after TLS/auth, LIST attributes/delimiter, SELECT FLAGS/PERMANENTFLAGS/UIDVALIDITY, UIDPLUS receipts and IDLE notifications. No mutation enabling from a version string alone.
4. **Coverage and identity:** record one known message in its folder, two labels, All Mail and Starred; compare provider header IDs and independently scoped UIDs. Include Spam, Trash, drafts, label-only/orphan-like mail and recovered local mail. Check All Mail visibility/exclusion settings.
5. **Epoch lifecycle:** repeat the identity inventory after ordinary restart and ordinary logout/login. Qualify mode switches and repair separately. Avoid using account removal/reset as an initial diagnostic: it destroys recovery state. If tested, export Recovered Messages first.
6. **MIME:** compare acquired literals and decoded attachments before/after rebuild; include PGP signed/encrypted mail, S/MIME, HTML/CID parts, multipart/alternative, nested messages, bad charsets and undisclosed-recipient groups. Distinguish harmless boundary drift from lost content.
7. **Performance:** measure first inbox, headers/body backfill, search coverage, warm restart, RAM/disk use and p50/p95 new-mail latency. Test while Bridge is still syncing and when IDLE is unavailable.
8. **Mutations:** test the operation matrix using expendable mail; verify results in Proton’s web UI and another client. Include flag conflicts, source/destination already equal, lost completion, add-label preserving folder, Trash retaining/removing labels and undo.
9. **SMTP:** primary and secondary identities, custom-domain/plus identities actually used by the owner, To/Cc/Bcc, attachments and boundary sizes; one SMTP submission produces the expected Sent copy without Sweetbriar APPEND. Simulate lost final completion in the fixture, then verify real recovery diagnostics without blindly resending.

Acceptance requires an inventory/byte reconciliation report and operation-specific traces. It does not require measuring or changing every message in the owner’s account. The destructive operations deferred from the product scope can stay fixture-only until a later feature gate.

## 3. Options compared

### Integration and testing options

| Option | Benefits | Costs/limits | Recommendation |
|---|---|---|---|
| User-installed official Bridge; standard IMAP/SMTP | Documented route; Proton owns account authentication and crypto. | Separate process, paid Mail plan, local cache, feature/version-dependent behavior. | **First live backend.** |
| Bundle/automatically manage Bridge | Potentially smoother onboarding. | Packaging, updates, GPL redistribution and lifecycle ownership become Sweetbriar obligations. | Defer. |
| Direct Proton service API | Potentially richer identity/crypto semantics and one process. | Outside this documented Bridge integration; separate authorization and maintenance questions. | Outside scope. |
| Dovecot-only CI | Fast, familiar generic server fixture. | Does not validate Proton mutation or MIME semantics. | Necessary but insufficient. |
| Dovecot + scripted Proton imitation | Deterministic edge cases. | An imitation can encode our own mistaken understanding. | Use for adversarial faults, not sole ground truth. |
| Real Bridge + mock API | Actual Proton connector and Gluon code without credentials. | Go/native build, harness adaptation, no production-service fidelity guarantee. | Add before enabling filing/send. |

The factual distinctions are established in [Proton’s integration documentation](https://proton.me/support/imap-smtp-and-pop3-setup), [copying notes](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/COPYING_NOTES.md), and [mock/core fixtures](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/ctx_bridge_test.go). The choices in the last column are recommendations.

### First-backend scope

| Slice | Included | Deferred | Evidence to advance |
|---|---|---|---|
| A: Read-only | One account, combined mode, all visible mailbox occurrences, resumable headers/bodies, local search/export, local drafts, health/coverage. | Remote flags, filing, imports, submission. | Restart/repair/partial-sync retention; identity/MIME corpus; inventory reconciliation. |
| B: Flags | Explicit Seen and Flagged deltas. | Arbitrary Proton keywords, broad Answered/Draft writes. | Cross-mailbox/provider convergence and external-client conflict traces. |
| C: Filing | Qualified folder moves, archive, Trash, add-label, conditional undo. | Generic COPY-delete, pure label removal, permanent expunge, cross-account moves, folder administration. | Every allowed source/destination class and interruption boundary qualified. |
| D: Send | Frozen Out Box attempts, verified sender identities, plain/HTML MIME and attachments, SMTP, Sent reconciliation. | Server draft replacement, scheduled-server send, automatic unknown retry. | DATA/final-reply drops, crash/restore proof and real Sent/identity tests. |

## 4. Recommendations for Sweetbriar and strongest counter-arguments

**1. Keep a generic Rust IMAP transport with an explicit Proton Bridge policy layer.** Do not put label semantics in the generic protocol parser or macOS UI. The backend should declare multi-membership, copy-to-folder-may-move, automatic-Sent, restricted keyword support, local reachability and unsupported mutation classes. The native shell supplies credential/certificate interaction and account health views; the shared core owns interpretation and durability.

The existing project prior of a thin client over **`imap-next`/`imap-codec`**, TLS through **`rustls`**, and SMTP through **`lettre`** remains plausible. Their current documentation exposes sans-I/O state progression, codecs, certificate-verifier hooks and asynchronous SMTP transport. This is a candidate implementation route, not a library compatibility certification. Make the raw-UTF8 ENVELOPE case and custom certificate handling acceptance tests before pinning dependencies. [imap-next](https://docs.rs/imap-next/latest/imap_next/), [imap-codec](https://docs.rs/imap-codec/latest/imap_codec/), [rustls verifier API](https://docs.rs/rustls/latest/rustls/client/danger/trait.ServerCertVerifier.html), [lettre transport](https://docs.rs/lettre/latest/lettre/transport/smtp/struct.AsyncSmtpTransport.html).

**Strongest counter-argument:** a dedicated Bridge-only client might be simpler than a reusable abstraction. Keep the policy surface small and implement only observed differences; do not design four future backends now.

**2. Treat identity association, acquired bytes and mutation authority as separate evidence.** Account-scoped provider headers associate label appearances; immutable local objects preserve downloads; mailbox epochs/UIDs authorize remote commands. Correspondence across repair can improve display and search without reviving stale deletions. Missing/malformed identity must remain visible rather than being “fixed” by Message-ID merging.

**Strongest counter-argument:** this complicates reconciliation and may temporarily show duplicates. That is preferable to destroying a distinct occurrence. Collapse duplicates only after qualified provider identity evidence, retaining reversible association history.

**3. Start with read-only acquisition and local drafts.** Use EXAMINE where suitable and BODY.PEEK to avoid setting Seen while reading. Acquire bounded UID batches, publish durable checkpoints and retain the prior inventory through connection failures. Sweep flags periodically because there is no CONDSTORE. Add remote read/star only after independent evidence. EXAMINE and BODY.PEEK behavior are specified in [IMAP4rev1](https://www.rfc-editor.org/rfc/rfc3501).

**Strongest counter-argument:** a read-only inbox cannot replace the owner’s daily client. Make it a finite qualification slice with the B/C/D gates above, not an indefinite archive detour.

**4. Keep Proton filing commands semantic.** Add label uses qualified COPY-to-label. Archive/file/Trash uses a qualified existing occurrence and UID MOVE. Same-folder filing is a local no-op. Do not substitute APPEND/delete or assume COPY is harmless. Prefer refusing an unsupported source/destination pair over quietly performing a different operation. Fetch and retain complete source content before filing; verify resulting membership/provider identity before completing the durable intent.

From a label or aggregate search result, resolve the appropriate occurrence explicitly. Do not silently choose a different folder for a stale operation. Undo is another preconditioned operation: if another client has since moved the message, preserve that change and report the conflict.

**Strongest counter-argument:** these restrictions make familiar drag/drop and label removal incomplete. Expose supported actions clearly, and add the missing semantic operations when their qualification cases pass. They should not be implemented by accidentally destructive generic commands.

**5. Make submission independent of Sent synchronization.** Store submitted MIME, identity, recipients and attempt phases durably. Bridge owns server Sent filing. A delayed Sent view becomes a reconciliation issue, not another APPEND or another send. Known pre-submission failures can be retried under policy; uncertainty after possible acceptance is a distinct state requiring inspection.

**Strongest counter-argument:** users expect automatic retries, and Bridge already suppresses duplicates. Its in-memory recorder cannot meet Sweetbriar’s crash/restore promise. The inspectable Out Box should explain and resolve uncertainty without manufacturing exactly-once guarantees.

**6. Keep a small dated Bridge compatibility ledger.** Record official binary version, OS build, actual capabilities, mailbox role/flag observations, TLS fingerprint, allowed mutation matrix and last qualification. Keep credentials/content out of routine diagnostic logs. After an update or configuration change, reacquire capabilities and inventory and recheck write permissions before replaying pending intents.

**Strongest counter-argument:** requalification increases maintenance and can interrupt use after updates. Read-only operation can continue when safe; disable only operations whose targeting or semantics are uncertain. Keep the ledger focused on the narrow first-backend scope.

**Overall strongest argument against Proton first:** Bridge adds another sync engine, a separate lifecycle and cache, unusual filing semantics and reconstruction limits. A conventional IMAP provider would be an easier engineering baseline, while provider-specific work may delay the native experience. The counterweight is the project’s explicit owner-provider gate and daily access to real evidence. Preserve a generic Dovecot lane, keep Proton mutations narrow, and use dogfooding to earn expansion rather than promise broad provider parity.

## 5. Risks and open questions

- **macOS 27 qualification:** Proton’s published support matrix does not yet list it. The recent crash fix is encouraging but is not a substitute for the official binary running on the owner’s exact Mac. [Requirements](https://proton.me/support/operating-systems-supported-bridge), [3.26.0 notes](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.26.0).
- **Remote completeness:** Bridge-visible IMAP may omit failed builds, excluded Spam/Trash or hidden aggregate views. Decide how the UI distinguishes acquisition coverage from a fully reconciled Proton archive; qualify against owner-controlled inventories. [Build stage](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/syncservice/stage_build.go), [folder settings](https://proton.me/support/using-folders-labels).
- **Identity longevity:** `X-Pm-Internal-Id` is useful implementation evidence, not a published permanent identifier guarantee. Test ordinary relogin, repair, imported mail and address-mode changes; retain ambiguous records.
- **Feature-flag semantics:** folder-unlabel behavior and IDLE can vary beyond the version number. There is no promise that a public build tag reveals the owner’s current flag assignment. [Connector](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/connector.go), [session capabilities](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/internal/session/session.go).
- **Original MIME/crypto fidelity:** exact sender-original MIME and signature results across all formats remain **(unverified)**. The corpus must include cryptographic and attachment preservation, with clear export provenance.
- **SMTP ambiguity and alias scope:** test the owner’s actual secondary/custom-domain/plus/reverse-alias workflows; late DATA errors and lost completion must not cause duplicate delivery. [Submission path](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/smtp/smtp.go).
- **Local disk and recovery:** plan for Bridge’s cache plus Sweetbriar’s immutable archive and index, with headroom during rebuild. Recovered Messages is irreplaceable local data until independently exported. [Cache report](https://github.com/ProtonMail/gluon/issues/452), [recovery documentation](https://proton.me/support/what-is-the-recovered-messages-folder-in-bridge).
- **Test/production divergence:** mock API behavior, shortened test polling and mocked keychains do not establish production-service or macOS behavior. Run both the credential-free lane and the manual official-binary gate. [Core test setup](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/ctx_bridge_test.go), [test timing overrides](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/main_test.go).
- **Policy/support continuity:** the October terms explicitly reserve service/feature changes. Keep independent reading/export/recovery usable if Bridge availability or eligibility changes. [Terms](https://proton.me/legal/terms).

## 6. Sources consulted

All sources linked in the findings were consulted on **2026-10-12**. Downloaded primary evidence and source snapshots are retained under `evidence/`. HTTP-source snapshots were fetched without using git. The archived Bridge issue was used because its live page returned 404. The following register groups the consulted URLs used by the report.

**Tagged Bridge implementation and tests**

- [go.mod](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/go.mod).
- [internal/vault/types_settings.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/vault/types_settings.go).
- [pkg/message/build.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/pkg/message/build.go).
- [internal/services/imapservice/imap_updates.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/imap_updates.go).
- [tests/features/imap/message/copy.feature](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/copy.feature).
- [tests/features/imap/message/move.feature](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/move.feature).
- [internal/services/sendrecorder/recorder.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/sendrecorder/recorder.go).
- [internal/services/imapservice/sync_build.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/sync_build.go).
- [tests/api_test.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/api_test.go).
- [tests/ctx_bridge_test.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/ctx_bridge_test.go).
- [README.md](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/README.md).
- [internal/app/frontend.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/app/frontend.go).
- [BUILDS.md](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/BUILDS.md).
- [internal/constants/constants.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/constants/constants.go).
- [internal/services/imapsmtpserver/listener.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapsmtpserver/listener.go).
- [internal/services/imapsmtpserver/smtp.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapsmtpserver/smtp.go).
- [internal/certs/tls.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/certs/tls.go).
- [internal/frontend/grpc/service_cert.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/frontend/grpc/service_cert.go).
- [tests/features/imap/mailbox/list.feature](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/mailbox/list.feature).
- [internal/services/imapservice/helpers.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/helpers.go).
- [internal/services/imapservice/service.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/service.go).
- [tests/features/user/sync.feature](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/user/sync.feature).
- [tests/features/user/relogin.feature](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/user/relogin.feature).
- [internal/services/imapservice/connector.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/connector.go).
- [tests/features/imap/message/store.feature](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/store.feature).
- [tests/features/imap/message/delete.feature](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/delete.feature).
- [tests/features/imap/message/delete_from_trash.feature](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/delete_from_trash.feature).
- [tests/features/imap/message/create.feature](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/imap/message/create.feature).
- [tests/features/smtp/send/send_append.feature](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/features/smtp/send/send_append.feature).
- [Changelog.md](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/Changelog.md).
- [pkg/message/build_test.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/pkg/message/build_test.go).
- [pkg/message/decrypt.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/pkg/message/decrypt.go).
- [internal/services/syncservice/stage_build.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/syncservice/stage_build.go).
- [internal/services/imapservice/service_message_events.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapservice/service_message_events.go).
- [internal/services/smtp/smtp.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/smtp/smtp.go).
- [internal/services/smtp/smtp_backend.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/smtp/smtp_backend.go).
- [internal/services/smtp/errormapper.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/smtp/errormapper.go).
- [internal/services/imapsmtpserver/imap.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/imapsmtpserver/imap.go).
- [internal/services/syncservice/limits.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/syncservice/limits.go).
- [internal/user/user.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/user/user.go).
- [internal/services/smtp/accounts.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/internal/services/smtp/accounts.go).
- [COPYING_NOTES.md](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/COPYING_NOTES.md).
- [tests/README.md](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/README.md).
- [Makefile](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/Makefile).
- [tests/bdd_test.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/bdd_test.go).
- [tests/main_test.go](https://raw.githubusercontent.com/ProtonMail/proton-bridge/v3.27.1/tests/main_test.go).

**Proton documentation and packaging**

- [Bridge product page](https://proton.me/mail/bridge).
- [Stable release notes](https://proton.me/download/bridge/stable_releases.html).
- [certificate guidance](https://proton.me/support/bridge-ssl-connection-issue).
- [Label documentation](https://proton.me/support/labels-in-bridge).
- [Proton setup instruction](https://proton.me/support/protonmail-bridge-clients-macos-outlook-2019).
- [Proton requirements](https://proton.me/support/operating-systems-supported-bridge).
- [Linux packages](https://proton.me/support/bridge-for-linux).
- [Flathub package](https://flathub.org/en/apps/ch.protonmail.protonmail-bridge).
- [IMAP/SMTP introduction](https://proton.me/support/imap-smtp-and-pop3-setup).
- [Installation guide](https://proton.me/support/protonmail-bridge-install).
- [Autostart documentation](https://proton.me/support/automatically-start-bridge).
- [Proton’s CLI guide](https://proton.me/support/bridge-cli-guide).
- [Settings guide](https://proton.me/support/comprehensive-guide-to-bridge-settings).
- [port troubleshooting](https://proton.me/support/port-already-occupied-error).
- [Certificate explanation](https://proton.me/support/macos-certificate-warning).
- [Bridge security model](https://proton.me/blog/bridge-security-model).
- [client credential instructions](https://proton.me/support/protonmail-bridge-clients-windows-outlook-2019).
- [Address-mode documentation](https://proton.me/support/difference-combined-addresses-mode-split-addresses-mode).
- [additional-address guide](https://proton.me/support/mail-bridge-addresses).
- [Folder/label documentation](https://proton.me/support/using-folders-labels).
- [Recovery documentation](https://proton.me/support/what-is-the-recovered-messages-folder-in-bridge).
- [attachment limits](https://proton.me/support/attaching-multiple-documents-to-a-message).
- [Delay guidance](https://proton.me/support/how-to-troubleshoot-messages-received-with-a-delay-in-your-email-client).
- [connection behavior](https://proton.me/support/how-to-resolve-connection-issues-in-bridge).
- [Business SMTP documentation](https://proton.me/support/smtp-submission).

**Releases, repository metadata and issues**

- [GitHub release](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.27.1).
- [release metadata](https://api.github.com/repos/ProtonMail/proton-bridge/releases/latest).
- [v3.26.0 release](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.26.0).
- [Gmail-extension PR](https://github.com/ProtonMail/gluon/pull/450).
- [SORT/THREAD issue](https://github.com/ProtonMail/gluon/issues/378).
- [Archived issue and maintainer discussion](https://web.archive.org/web/20221203013158/https://github.com/ProtonMail/proton-bridge/issues/220).
- [v3.24.1 release](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.24.1).
- [v3.25.0 release](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.25.0).
- [Gluon #498](https://github.com/ProtonMail/gluon/issues/498).
- [Gluon #482](https://github.com/ProtonMail/gluon/issues/482).
- [Gluon #452](https://github.com/ProtonMail/gluon/issues/452).
- [Gluon #471](https://github.com/ProtonMail/gluon/issues/471).
- [Gluon #386](https://github.com/ProtonMail/gluon/issues/386).
- [Bridge 3.27.0](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.27.0).
- [current metadata](https://api.github.com/repos/ProtonMail/proton-bridge).

**Pinned Gluon implementation**

- [gluon/imap/capabilities.go](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/imap/capabilities.go).
- [gluon/internal/session/session.go](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/internal/session/session.go).
- [gluon/tests/capability_test.go](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/tests/capability_test.go).
- [gluon/internal/ids/header.go](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/internal/ids/header.go).
- [gluon/imap/uid_validity_generator.go](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/imap/uid_validity_generator.go).
- [gluon/internal/backend/connector_updates.go](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/internal/backend/connector_updates.go).
- [gluon/internal/state/updates.go](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/internal/state/updates.go).
- [gluon/builder.go](https://raw.githubusercontent.com/ProtonMail/gluon/7e800978ab4a/builder.go).

**Standards, fixture and Rust API documentation**

- [RFC 4315](https://www.rfc-editor.org/rfc/rfc4315).
- [MOVE protocol](https://www.rfc-editor.org/rfc/rfc6851).
- [RFC 2177](https://www.rfc-editor.org/rfc/rfc2177).
- [Virtual plugin](https://doc.dovecot.org/2.4.5/core/plugins/virtual.html).
- [imaptest](https://dovecot.github.io/imaptest/).
- [imap-next](https://docs.rs/imap-next/latest/imap_next/).
- [imap-codec](https://docs.rs/imap-codec/latest/imap_codec/).
- [rustls verifier API](https://docs.rs/rustls/latest/rustls/client/danger/trait.ServerCertVerifier.html).
- [lettre transport](https://docs.rs/lettre/latest/lettre/transport/smtp/struct.AsyncSmtpTransport.html).
- [IMAP4rev1](https://www.rfc-editor.org/rfc/rfc3501).

**Pinned Proton API dependency**

- [go-proton-api/message_import.go](https://raw.githubusercontent.com/ProtonMail/go-proton-api/8a62023d3e38/message_import.go).
- [go-proton-api/ticker.go](https://raw.githubusercontent.com/ProtonMail/go-proton-api/8a62023d3e38/ticker.go).
- [go-proton-api/response.go](https://raw.githubusercontent.com/ProtonMail/go-proton-api/8a62023d3e38/response.go).

**Pinned SMTP dependency**

- [go-smtp/conn.go](https://raw.githubusercontent.com/ProtonMail/go-smtp/2b3d50599865/conn.go).

**Terms and licensing**

- [Current terms](https://proton.me/legal/terms).
- [archived terms](https://web.archive.org/web/20261003105539id_/https://proton.me/legal/terms).
- [FSF aggregation guidance](https://www.gnu.org/licenses/gpl-faq.html#MereAggregation).

