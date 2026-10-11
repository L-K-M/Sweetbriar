# Email Client Market Survey — Privacy & Team Notes

**Date:** 2026-10-11
**Scope:** Proton Mail, Fastmail, HEY, Missive, Front

---

## 1. Proton Mail (proton.me)

**Platforms & Desktop Tech:** Web app, native desktop apps for macOS (Apple Silicon + Intel), Windows (x64), Linux (beta). Mobile: iOS, Android. Native per-platform builds (not Electron). Bridge (paid only) provides local IMAP/SMTP for Outlook/Thunderbird/Apple Mail.

**Pricing (USD):** Free: $0, 1 GB, 1 address, 150 msg/day. Mail Plus: $3.99/mo (annual, 15 GB, 10 addresses, 1 domain). Proton Unlimited: $9.99/mo (annual, 500 GB, 15 addresses, 3 domains, +VPN/Drive/Pass). 30-day money-back.

**Account Backends:** Own encrypted mail service only. No external IMAP/Gmail/Exchange proxying. Easy Switch imports from Gmail/Outlook/Yahoo.

**Standout Features:** Zero-access E2E encryption (Proton cannot read mail). Swiss jurisdiction. PhishGuard, tracker blocking, hide-my-email aliases. Proton Scribe AI writing assistant (add-on). Open source, audited.

**Privacy Model:** Mail stored encrypted on Swiss servers. Subscriber-funded, no ads/tracking.

**AI:** Proton Scribe (writing assistant, add-on). Lumo AI (separate product).

---

## 2. Fastmail (fastmail.com)

**Platforms & Desktop Tech:** Web app, native desktop apps for Windows (x64), macOS (Apple Silicon + Intel), Linux (Flathub). Mobile: iOS, Android. Native apps with offline support. Works with any IMAP client (Standard/Professional).

**Pricing (EUR):** Paid only, no free tier. 30-day trial. Individual: €5/mo (annual, 60 GB). Duo: €8/mo (2 inboxes, 120 GB). Family: €11/mo (up to 6 users, 360 GB). Business: Basic €3/user/mo, Standard €5/user/mo, Professional €9/user/mo. Tax-inclusive.

**Account Backends:** Own mail service. Custom domains supported. No external account proxying. Migration tools for Gmail/Outlook/Yahoo/Proton/HEY.

**Standout Features:** Masked Email (unique address per login). Image proxy blocking (spy pixel prevention). Scheduled Send, Snooze, Mute. JMAP support. Data residency choice (EU or USA). Offline support. Private memos.

**Privacy Model:** No ads/tracking/mining. Australian company. Employee access logged and audited. Responds only to valid Australian warrants. Transparency report.

**AI:** None observed as of 2026-10-11.

---

## 3. HEY by 37signals (hey.com)

**Platforms & Desktop Tech:** Web app, native desktop apps for macOS (Apple Silicon + Intel), Windows (Microsoft Store), Linux (Snap). Mobile: iOS, Android (separate Email + Calendar apps). Native apps.

**Pricing (USD):** Paid only. 30-day trial, no CC required. HEY for You: $99/yr (personal, @hey.com address). HEY for Domains: $12/user/mo (first user $10/mo). HEY for Families: $179/yr (up to 5 people). Ultra-short addresses: 2-char $999/yr, 3-char $349/yr. USD only, annual billing for personal.

**Account Backends:** Own mail service only. No external IMAP/Gmail/Exchange support. Custom domains via HEY for Domains plan.

**Standout Features:** The Screen (sender approval). Imbox/Paper Trail/Feed (purpose-built boxes). HEY Calendar (included). Spy pixel blocking. HEY World (email-the-web blog). AI Agents & CLI (new). HEY TUI (terminal UI, new). 2FA required (TOTP + WebAuthn). Thread sharing, private team comments.

**Privacy Model:** No ads/data selling. Encryption at-rest, at-work (per-field DB encryption), in-transit (TLS). NOT end-to-end. Employee access limited, justified, audited weekly. External audits (Trail of Bits, Doyensec). US company (37signals LLC).

**AI:** AI Agents & CLI (new). HEY TUI (new). No built-in writing assistant observed.

---

## 4. Missive (missiveapp.com)

**Platforms & Desktop Tech:** Web app, native desktop apps for macOS (.dmg) and Windows (.exe). Mobile: iOS/iPadOS, Android. Native apps. Version 10.64.1 observed.

**Pricing (USD):** SaaS per-user. 30-day trial (Productive plan). Starter: $14/user/mo (annual, up to 5 users). Productive: $24/user/mo (up to 50 users, integrations, API/MCP). Business: $36/user/mo (unlimited users, SAML SSO, IP restriction). 20% annual discount.

**Account Backends:** Supports external accounts: Gmail, Google Workspace, Outlook/Office 365, any IMAP. Also WhatsApp, Messenger, Instagram, SMS (Twilio/Quo/Dialpad). Up to 10 personal + 5 shared accounts per user. Does not provide its own mail service.

**Standout Features:** Shared team inboxes with internal threads. Collaborative drafting, assignment, read statuses. Tasks from emails. Rules/automations (up to 1000). 25+ integrations. AI assistant with MCP. SOC 2 Type II, GDPR ready.

**Privacy Model:** SOC 2 Type II. AI data routed through chosen provider (Anthropic/OpenAI/Google). Missive does not train on emails. AI fully opt-out. Web search disabled for AI (prompt injection prevention).

**AI:** AI assistant (conversational sidebar: search, draft, summarize, translate). Multi-provider (Claude, GPT, Gemini). Missive AI credits or BYOK. MCP integrations (Stripe, Notion, Linear, etc.). AI rules. Productive/Business plans.

---

## 5. Front (front.com)

**Platforms & Desktop Tech:** Web app, native desktop apps for macOS (.dmg universal) and Windows (.exe). Mobile: iOS, Android. Native apps.

**Pricing (USD):** SaaS per-seat. 14-day trial (Professional features). Starter: $25/seat/mo (up to 10 seats, single channel). Professional: $65/seat/mo (up to 50 seats, omnichannel, SSO/SCIM). Enterprise: $105/seat/mo (unlimited rules, AI Copilot/QA/CSAT included). AI add-ons: Autopilot from $0.05/conversation, Copilot $20/seat/mo. Annual saves 24%.

**Account Backends:** Supports external email: Office 365, Gmail, custom (IMAP). Also SMS, social, WhatsApp (paid add-on), Slack, live chat, voice. Does not provide its own mail service.

**Standout Features:** Omnichannel shared inbox. Internal comments, shared drafts, assignments. Ticketing, knowledge base, 160+ integrations. AI Autopilot (AI agent). AI Copilot (suggested replies). Smart QA, Smart CSAT, AI Topics. External AI agents (Claude, Codex, Sierra). SOC 2 Type II, ISO 27001, GDPR, CCPA, HIPAA. AES-256.

**Privacy Model:** SOC 2 Type II, ISO 27001, GDPR, CCPA, HIPAA. AES-256 at rest, TLS in transit. AI providers (OpenAI, Azure OpenAI, AWS/Mistral) do not retain/train on data per agreements. US company.

**AI:** Autopilot (AI agent, $0.05/conversation). Copilot (drafting assistant, $20/seat/mo or included in Enterprise). Smart QA (AI quality scorecards). Smart CSAT (inferred satisfaction). AI Topics (contact reason analysis). External AI agents. Compass (agent performance monitoring). MCP server.

---

## Cross-Cutting Summary

| Dimension | Proton Mail | Fastmail | HEY | Missive | Front |
|---|---|---|---|---|---|
| Own mail service | Yes | Yes | Yes | No (client) | No (platform) |
| External IMAP/Gmail/Exchange | No (import only) | No (import only) | No | Yes | Yes |
| Free tier | Yes (1 GB) | No (30-day trial) | No (30-day trial) | No (30-day trial) | No (14-day trial) |
| Desktop tech | Native | Native | Native | Native | Native |
| E2E encryption | Yes (zero-access) | No | No | No | No |
| AI writing assistant | Scribe (add-on) | None observed | None observed | Built-in assistant | Copilot (add-on/Ent.) |
| AI agents | No | No | Agents & CLI (new) | MCP + AI rules | Autopilot + External |
| Privacy jurisdiction | Switzerland | Australia | US | — | US |
| Entry paid price | $3.99/mo | €5/mo | $99/yr | $14/user/mo | $25/seat/mo |

---

## SOURCES

- Proton Mail: https://proton.me/mail · https://proton.me/mail/pricing · https://proton.me/mail/download · https://proton.me/mail/bridge (all observed 2026-10-11)
- Fastmail: https://www.fastmail.com/about/ · https://www.fastmail.com/pricing/ · https://www.fastmail.com/download/ · https://www.fastmail.com/features/privacy/ (all observed 2026-10-11)
- HEY: https://www.hey.com/ · https://www.hey.com/pricing/ · https://www.hey.com/apps/ · https://www.hey.com/security/ (all observed 2026-10-11)
- Missive: https://missiveapp.com/ · https://missiveapp.com/pricing · https://missiveapp.com/download · https://missiveapp.com/ai-assistant (all observed 2026-10-11)
- Front: https://front.com/ · https://front.com/pricing · https://front.com/download · https://front.com/product/ai (all observed 2026-10-11)
