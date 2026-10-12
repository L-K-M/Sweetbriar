# Research validation

2026-10-12. Root checked helper findings against primary documentation and source.

- Devin Microsoft/MDM memo: corrected stale MSAL macOS-only WKWebView assertion using current customization documentation; qualified Rust HTTP interception versus browser SSO; confirmed ManagedAppConfigurationProvider macOS 27 metadata and WWDC26 deployment material.
- Devin KDE/precedents memo: corrected assertion that KOnlineAccounts has no PIM consumers; current README and 2025-11-15 developer article list PIM consumers in master, while retaining prototype status. Legacy Akonadi contacts/calendar integration is distinguished from mail.
- GOA Step-5 research helper ceased producing progress before a final memo and was stopped after the report research was complete. Root independently fetched and inspected GOA 3.58.1 release archive, tagged introspection XML/provider/build source, GOA goals, Geary mediator, EDS integration, Flatpak manifests and snapd interface source. Partial helper results were checked rather than accepted as authority.
- Confirmed standalone Windows Live removal from NEWS and current provider tree despite stale service table. Confirmed Google full-mail scope survives Google Files disablement.
- All report web citations have entries in its source index. Six required sections present; nine TLDR bullets; Markdown list and heading spacing checked.
- No macOS runtime, authenticated GOA/provider flow or MSAL tenant qualification performed. Those remain explicitly unverified, not represented as tests passed.
