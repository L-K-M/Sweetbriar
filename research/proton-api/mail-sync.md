# R3 — Mail data model, sync and mutations

References (read-only clones under `src/`): `go-proton-api` @ 79fbe06 (primary), `hydroxide` @ a08bcca (second), `proton-bridge` @ 36d7b6a (GPL-3.0; protocol facts only, no code copied). API base paths are `/core/v4` and `/mail/v4` (`go-proton-api/server/router.go:32,84`). Proton API booleans are `0`/`1` (`go-proton-api/boolean.go:8-14`).

Caveat: many endpoint details below (defaults, rejection rules, event shapes, import limits) come from `go-proton-api`'s client and its development server, not from Proton's production API. They are protocol evidence to qualify, not production guarantees.

## 1. Labels, folders, system labels

System label IDs (`go-proton-api/label_types.go:8-21`):

| Const | ID | Name shown by test server |
|---|---|---|
| InboxLabel | `0` | Inbox |
| AllDraftsLabel | `1` | AllDrafts |
| AllSentLabel | `2` | AllSent |
| TrashLabel | `3` | Trash |
| SpamLabel | `4` | Spam |
| AllMailLabel | `5` | All Mail |
| ArchiveLabel | `6` | Archive |
| SentLabel | `7` | Sent |
| DraftsLabel | `8` | Drafts |
| OutboxLabel | `9` | Outbox |
| StarredLabel | `10` | Starred |
| AllScheduledLabel | `12` | Scheduled |

Names from `go-proton-api/server/backend/api.go:323-335`; `11` is unused. Hydroxide's v3 constants agree for 0-8 and 10 (`hydroxide/protonmail/labels.go:7-18`).

`LabelType`: `1=Label`, `2=ContactGroup`, `3=Folder`, `4=System` (`go-proton-api/label_types.go:83-89`). `Label` fields: `ID, ParentID, Name, Path []string, Color, Type`; JSON `Path` is a `/`-joined string (`label_types.go:23-66`). User labels are type 1; folders are type 3 and may have `ParentID`; labels may not have children (Bridge rejects child labels, `proton-bridge/internal/services/imapservice/connector.go:760-763`). Endpoints: `GET /core/v4/labels?Type=N` (repeatable), `POST /core/v4/labels`, `PUT /core/v4/labels/{id}`, `DELETE /core/v4/labels/{id}` (`go-proton-api/label.go:28-80`; routes `server/router.go:59-65`). Create takes `Name, Color, Type, ParentID?`; update takes `Name, Color, ParentID?` (`label_types.go:68-81`). Deleting a label deletes descendant labels (`server/backend/api.go:1104-1112`).

Folder/label exclusivity: the import API counts labels with `Type != LabelTypeLabel` as "exclusive" and rejects more than one (`go-proton-api/server/messages.go:424-438`); Bridge treats a move whose destination is a folder or system label as expunging the source mailbox (`proton-bridge/internal/services/imapservice/connector.go:633-649,663-681`). Inference: a message belongs to at most one folder/system location, but may carry many user labels plus `AllMail`/`Starred`.

## 2. Message model

`MessageMetadata` (`go-proton-api/message_types.go:14-36`): `ID`, `AddressID`, `LabelIDs []string`, `ExternalID`, `Subject`, `Sender *mail.Address`, `ToList/CCList/BCCList/ReplyTos []*mail.Address`, `Flags MessageFlag`, `Time int64` (unix s), `Size int`, `Unread Bool`, `IsReplied`, `IsRepliedAll`, `IsForwarded`, `NumAttachments int`. Helpers: `Seen()` = !Unread; `Starred()` = LabelIDs contains `10`; `IsDraft()` = neither Received nor Sent flag (`message_types.go:38-48`). `Flags` is a bitfield: Received=1<<0, Sent=1<<1, Internal=1<<2, E2E=1<<3, Auto=1<<4, Replied=1<<5, RepliedAll=1<<6, Forwarded=1<<7, AutoReplied=1<<8, Imported=1<<9, Opened=1<<10, ReceiptSent=1<<11, Notified=1<<12, Touched=1<<13, Receipt=1<<14, ReceiptRequest=1<<16, PublicKey=1<<17, Sign=1<<18, Unsubscribed=1<<19, ScheduledSend=1<<20, Alias=1<<21, DMARCPass/SPFFail/DKIMFail/DMARCFail=1<<23..26, HamManual/SpamAuto/SpamManual/PhishingAuto/PhishingManual=1<<27..31 (`message_types.go:71-106`).

`Message` embeds `MessageMetadata` plus `Header string`, `ParsedHeaders Headers`, `Body string` (armored PGP for encrypted mail), `MIMEType rfc822.MIMEType`, `Attachments []Attachment` (`message_types.go:61-69`). `FullMessage` adds `AttData [][]byte` parallel to `Attachments` (`message_types.go:178-182`). `Headers` is `{Values map[string][]string; Order []string}` (used in `server/backend/message.go:249-266`).

ConversationID: **not present** in go-proton-api v4 `MessageMetadata` (grep finds no `Conversation` in the client). Hydroxide's v3 `Message` has `ConversationID` (`hydroxide/protonmail/messages.go:59`), a `Conversation` filter (`:163`), and `GET /conversations/{id}?MessageID=` (`hydroxide/protonmail/conversations.go:8-44`). Unverified: whether Proton's v4 API exposes conversation IDs or a `/mail/v4/conversations` endpoint.

`ExternalID` is the external Message-ID. Bridge writes `Message-Id: <ExternalID>` when the header lacks one, else `<ID@protonmail.internalid>` (`proton-bridge/pkg/message/build.go:472-482`); hydroxide uses ExternalID else `<ID@protonmail.com>` (`hydroxide/imap/message.go:21-27`).

`Attachment` (`go-proton-api/attachment_types.go:7-18`): `ID`, `Name`, `Size int64`, `MIMEType`, `Disposition` (`inline`/`attachment`, `:20-25`), `Headers`, `KeyPackets string` (base64), `Signature string`. System labels are synthesized into `LabelIDs`: `AllMail` always; `AllSent` if Sent/ScheduledSend; `AllDrafts` if no Sent/ScheduledSend/Received; `Starred` if starred; plus Inbox/Sent/Drafts/AllScheduled or the explicit system label (`server/backend/message.go:137-167`).

## 3. Listing, filtering, paging, counts

`MessageFilter` (`message_types.go:50-59`): `ID []string`, `Subject`, `AddressID`, `ExternalID`, `LabelID`, `EndID`, `Desc Bool`. Request body also carries `Page`, `PageSize`, `Sort`; client always sends `Sort:"ID"` (`message.go:294-329`). Page is 0-based; server default `Page=0`, `PageSize=100` (`server/messages.go:22-23`). Client page size constant `maxPageSize = 150` (`paging.go:13`) and it fetches `total/pageSize+1` pages concurrently (`paging.go:15-35`, `message.go:59-68`). Response includes `Stale` (0/1); client retries while stale (`message.go:294-328`). Inference: 150 is the client-chosen batch size, not a verified server maximum. Hydroxide v3 filter is richer: `Page, PageSize, Limit, Label, Sort, Asc, Begin, End, Keyword, To, From, Subject, Attachments, Starred, Unread, Conversation, AddressID, ID, ExternalID` (`hydroxide/protonmail/messages.go:146-167`).

`GET /mail/v4/messages/ids?AfterID=&Limit=` returns `{IDs:[...]}`; client clamps `Limit` to `maxMessageIDs = 1000` (`message.go:16,248-268`).

Counts: `GET /mail/v4/messages/count` → `Counts []{LabelID, Total, Unread}` (`message.go:331-343`, `message_types.go:218-222`, `server/router.go:106`). `CountMessages` does `POST /mail/v4/messages` with `X-HTTP-Method-Override: GET`, body = filter + `Limit:0`, and reads `Total` (`message.go:270-292`; server `server/messages.go:67-86`). Hydroxide v3 uses `GET /messages/count?Address=` (`hydroxide/protonmail/messages.go:222-241`).

## 4. Fetching a full message and attachments

`GET /mail/v4/messages/{id}` → `{Message}` (`message.go:41-53`; route `server/router.go:97`). `GetFullMessage` fetches the message, then downloads every attachment via `GET /mail/v4/attachments/{attachmentID}` using a scheduler, returning `AttData` in attachment order (`message.go:18-39`; `attachment.go:82-99` streams the raw body, no JSON parsing). Decrypt body with `Message.Decrypt`/`DecryptInto` (armored PGP → keyring) (`message_types.go:146-176`). Decrypt an attachment by base64-decoding `KeyPackets`, concatenating with the downloaded data packet, and decrypting the PGP message (`message_build.go:165-189`; same construction in `proton-bridge/internal/user/debug.go:348-380`). Upload: `POST /mail/v4/attachments` multipart with fields `MessageID, Filename, MIMEType, Disposition, ContentID` and blobs `KeyPackets, DataPacket, Signature`; client signs and encrypts the body first (`attachment.go:25-80`).

## 5. Events

`GET /core/v4/events/latest` → `{"EventID": "..."}` (`event.go:11-23`; `server/events.go:31-42`). Poll `GET /core/v4/events/{eventID}` → `Event` plus `More` (0/1) (`event.go:104-118`; `server/events.go:10-28`). If `More` is true, poll again using the last `EventID`; `GetEvent` collects at most `maxCollectedEvents = 50` events per call (`event.go:25-56`). `NewEventStream` polls on a ticker and skips a response whose last `EventID` is unchanged (`event.go:58-102`).

`RefreshFlag`: `RefreshMail = 1`; bits 1-7 unnamed; `RefreshAll = 255` (`event_types.go:77-89`). Hydroxide v3 defines `EventRefreshMail=1`, `EventRefreshContacts=2` (`hydroxide/protonmail/events.go:8-13`). What a client must do: treat refresh as invalidating local mail state and resync fully. Bridge's `HandleRefreshEvent` applies identity refresh, cancels sync, removes/rebuilds connectors, clears sync status, and restarts syncing from the event ID (`proton-bridge/internal/services/imapservice/service.go:291-325`); hydroxide resets all mailboxes and the messages DB and reinitializes mailboxes on `EventRefreshMail` (`hydroxide/imap/user.go:311-326`). The go-proton-api test backend drops all prior updates on `RefreshAll` (`go-proton-api/server/backend/backend.go:189-205`).

`Event` shape (`event_types.go:10-30`): `EventID`, `Refresh`, `User`, `UserSettings`, `MailSettings`, `Messages []MessageEvent`, `Labels []LabelEvent`, `Addresses []AddressEvent`, `Notifications []NotificationEvent`, `UsedSpace *int64`. `MessageEvent` = `{ID, Action, Message MessageMetadata}` (`:105-109`); `LabelEvent` = `{ID, Action, Label}` (`:111-115`); `AddressEvent` = `{ID, Action, Address}` (`:117-121`). `EventAction`: `0=Delete`, `1=Create`, `2=Update`, `3=UpdateFlags` (`:91-98`). v4 message events carry the full metadata, not a diff (built in `server/backend/api.go:1338-1449`). Counts are **not** in the v4 `Event` struct; hydroxide v3 events carry `MessageCounts []*MessageCount` (`LabelID, Total, Unread`) and per-message `LabelIDsAdded/Removed` diffs (`hydroxide/protonmail/events.go:15-65`). Unverified: whether the real v4 event payload includes a counts field that go-proton-api ignores.

## 6. Mutations

All actions take `{IDs:[...]}` (or `{LabelID, IDs}`) and are chunked by the client at `maxPageSize = 150` (`paging.go:13`; chunks at `message.go:88,100,150,168,184,217`).

| Action | Endpoint | Notes |
|---|---|---|
| read | `PUT /mail/v4/messages/read` | (`message.go:99-109`) |
| unread | `PUT /mail/v4/messages/unread` | (`message.go:167-179`) |
| forward/unforward | `PUT /mail/v4/messages/forward`, `/unforward` | per-message `{ID,Response}` (`message.go:111-165`) |
| label | `PUT /mail/v4/messages/label` | `{LabelID, IDs}`; response `Responses []LabelMessageRes` + `UndoToken` (`message.go:181-212`, `message_types.go:193-216`) |
| unlabel | `PUT /mail/v4/messages/unlabel` | same shape (`message.go:214-246`) |
| delete (permanent) | `PUT /mail/v4/messages/delete` | (`message.go:87-97`) |
| undo label ops | `POST /mail/v4/undoactions` | body = `UndoToken{Token, ValidUntil}`; expired tokens rejected (`undo.go:12-27`, `undo_types.go:5-13`) |

Star/unstar is label/unlabel with `StarredLabel` "10" (`proton-bridge/internal/services/imapservice/connector.go:707-713`). Trash is label with `TrashLabel` "3"; Bridge permanently deletes on expunge from Trash/Drafts when no other non-virtual label remains (`proton-bridge/internal/services/imapservice/connector.go:521-560`); permanent delete is `DeleteMessage` (`:505-512`). Move = label destination, then unlabel source when the source is a user label or the destination is folder/system; All Mail/Scheduled are not valid move endpoints (`proton-bridge/internal/services/imapservice/connector.go:617-697`). Hydroxide v3 also has `PUT /messages/undelete` (`hydroxide/protonmail/messages.go:332-338`); go-proton-api v4 has no undelete method. Unverified: v4 undelete/restore endpoint. Server rejects labeling with `AllMail`, `AllDrafts`, `AllSent` (`server/backend/api.go:627-629,661-663`).

## 7. Drafts and sending

`DraftTemplate` (`message_draft_types.go:9-20`): `Subject, Sender, ToList, CCList, BCCList, Body, MIMEType, Unread, ExternalID?`. `CreateDraftReq`: `Message, AttachmentKeyPackets, ParentID?, Action` where `Action` = 0 Reply, 1 ReplyAll, 2 Forward, 3 AutoResponse, 4 ReadReceipt (`:22-38`). `UpdateDraftReq`: `Message, AttachmentKeyPackets` (`:40-43`).

- Create draft: `POST /mail/v4/messages`; client encrypts `Body` with the address keyring and sends armored text (`message_send.go:11-40`).
- Update draft: `PUT /mail/v4/messages/{draftID}`; body re-encrypted if non-empty (`message_send.go:42-73`). Server requires `Sender`, `Sender.Address`, and `MIMEType` text/plain or text/html (`server/backend/api.go:758-770`).
- Send: `POST /mail/v4/messages/{draftID}` with `SendDraftReq{Packages []*MessagePackage}` → `{"Sent": Message}` (`message_send.go:75-87`; `message_send_types.go:98-100`).

`MessagePackage` = `{Addresses map[string]*MessageRecipient, MIMEType, Type, Body (base64 data packet), BodyKey?, AttachmentKeys?}`; `MessageRecipient` = `{Type, Signature, BodyKeyPacket?, AttachmentKeyPackets?}`; `SessionKey` = `{Key base64, Algorithm}` (`message_send_types.go:30-68`). Schemes: `Internal=1, EncryptedOutside=2, Clear=4, PGPInline=8, PGPMIME=16, ClearMIME=32` (`:11-20`). Sending marks the draft Sent, adds Sent label, and sets Replied/RepliedAll/Forwarded on the parent (`server/backend/api.go:795-828`).

## 8. Raw RFC 822: not available; must be reconstructed

No raw-RFC822/export endpoint exists in go-proton-api (no such route in `server/router.go:93-114`); the client reconstructs with `BuildRFC822(kr, msg, attData)` from `Header`/`Body`/decrypted attachments (`message_build.go:25-67`). Bridge's sync "build" stage decrypts `FullMessage` and converts to RFC822 (`proton-bridge/internal/services/syncservice/stage_build.go:59-61`), using `BuildRFC822Into` (`pkg/message/build.go:52-69`) with simple text, multipart/mixed (inline parts in multipart/related, attachments base64), and PGP/MIME signed/encrypted paths; header fields set include Subject, From, Reply-To, To, Cc, Bcc, Message-Id, Date, X-Pm-Internal-Id, X-Pm-External-Id, X-Pm-Date, References (`build.go:385-460`, options `options.go:20-27`). Hydroxide's exporter reads `msg.Header`, sets Content-Type/quoted-printable, and writes the decrypted body only (attachments are a TODO) (`hydroxide/exports/messages.go:18-51`); its IMAP layer builds multipart/mixed from the decrypted body plus attachment parts (`hydroxide/imap/message.go:245-300`). Inference: Proton's API does not expose the original raw RFC 822 message, so clients must rebuild it.

## 9. Import / upload

Message import: `POST /mail/v4/messages/import` multipart. Client first encrypts each RFC 822 literal with the address keyring (`EncryptRFC822`, `message_encrypt.go:24-32`), then sends one part per message named `0`, `1`, … (filename `N.eml`, content type `message/rfc822`) plus a `Metadata` JSON part keyed by the same names (`message_import_types.go:38-66`). `ImportMetadata` = `{AddressID, LabelIDs, Unread, Flags}` (`:26-31`). Limits: max 10 messages per request, max total encrypted size `MaxImportSize = 70 MiB` (`message_import.go:20-27,55-79`); the error text says 50 MB (`:30`). Response: `Code` 1001 (MultiCode) with `Responses []{Name, Response{Code, MessageID}}` (`message_import.go:81-124`; `response.go:22-24`). Server rejects `AllDrafts`/`AllSent`/`AllMail`/`Outbox` label IDs and more than one exclusive (folder/system) label, parses the RFC 822 into header/body/attachments, imports attachments, and applies labels without emitting events (`server/messages.go:415-466`). Hydroxide v3 uses the same `/import` shape with `message/rfc822` parts and a `Metadata` JSON part (`hydroxide/protonmail/import.go:38-54,87-159`).

Attachment upload is in §4.

## Open questions

- Does the v4 API expose `ConversationID` / a conversations endpoint? go-proton-api v4 has none; only hydroxide v3 does.
- Does the v4 event payload include message/conversation counts? go-proton-api's v4 `Event` has no counts field; hydroxide v3 does.
- Is there a v4 undelete/restore endpoint? v3 has `/messages/undelete`; v4 client does not.
- Actual server-side maximum `PageSize` for `/mail/v4/messages` (client uses 150; test server defaults to 100).
- Exact semantics of the other `Refresh` bits in v4 (only 1 and 255 are named in go-proton-api).
- Whether `PUT /mail/v4/messages/delete` is always permanent or is trash when the message is outside Trash; references show it used for permanent deletion after expunge, and trash is done by labeling with `TrashLabel`.
- Whether `ExternalID` is always present for received mail and how it relates to the `Message-ID` header when both exist.
- Real API behavior of `Stale` on message listing and the exact retry contract.
- Import size limit discrepancy: constant 70 MiB vs error string 50 MB (`go-proton-api/message_import.go:26,30`).
