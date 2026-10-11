<!-- provenance: helper=mimo model=cline-free/mimo-v2.6-flash started=2026-10-11T18:14:20+00:00 exit=1 brief=research/briefs/ -->

# Eudora (classic Mac and Windows email client) — Research Report

> Sweetbriar research series. Date: 2026-10-11. Status: IN PROGRESS (history drafted; features/reviews pending).

## 1. TL;DR

(coming)

## 2. Findings

### 2.1 History (verified; sources in §6)

**Origins (1988–1991, UIUC).** Eudora was written starting in 1988 by Steve Dorner, of the Computer Services Organization, University of Illinois at Urbana–Champaign ([Wikipedia](https://en.wikipedia.org/wiki/Eudora_(email_client))). Dorner (CHM interview): "I started Eudora in 1988, at the University of Illinois, about four years before I came to Qualcomm. We began it because the internet was a growing and burgeoning place, but email was not really established on the desktop computers that people were using at the time." First version took "just over a year": 50,000 lines of C, Classic Mac only, free ([CHM blog, 2018-05-22](https://computerhistory.org/blog/the-eudora-email-client-source-code/)). Named after Eudora Welty's story "Why I Live at the P.O."; slogan "Bringing the P.O. to Where You Live"; Welty was "pleased and amused" ([NYT 1997](https://www.nytimes.com/library/cyber/week/012197eudora.html), as quoted by CHM/Wikipedia).

**Qualcomm years (1991–2006).** Qualcomm licensed Eudora from UIUC in 1991; Dorner joined Qualcomm (working remotely from Illinois). PM John Noerenberg assigned Jeff Beckley and Jeff Gehlhaar to build the MS-DOS/Windows version; Mac and Windows remained separate codebases (C vs C++), different teams, different milestones ([CHM](https://computerhistory.org/blog/the-eudora-email-client-source-code/), [Wikipedia](https://en.wikipedia.org/wiki/Eudora_(email_client))). Internal-only at first ("I used to hate email. But I love Eudora!!"); consumer release in 1993; team peaked "over 50 people".

**Business model arc.** Free at first; Beckley ran it as "postcard-ware" ("I got thousands of postcards from all over the world"). Then commercialized: paid Pro at $19.95 with a free ad-supported version alongside; Pro "eventually sold for as much as $65" (CHM). Wikipedia: Light (freeware) + Pro (commercial); 2003–2006 Pro also as "Sponsored mode" (adware). "By 2001, over 100 person-years of development had been invested" (CHM). InfoWorld 1996-04-22: Eudora Pro 2.0 was Qualcomm's "best-selling product"; IDC: Qualcomm claimed 64.7% of e-mail software revenues in 1995; Qualcomm museum exhibit: "By 1996 Eudora had 18 million users" (all quoted in CHM). 1995: trademark licensed to WhoWhere? webmail (eudoramail.com, later Lycos; closed 2006) (Wikipedia).

**Decline and end (2006).** Qualcomm decided in 2006 Eudora "was no longer consistent with their other major project lines". CHM's analysis: Outlook preloaded and corporate standardization plus other free clients; "it was never going to be a business big enough to 'move the needle' for a company of Qualcomm's size". Final mainline releases: **Eudora 7.1.0.9 (Windows) and 6.2.4 (Mac), both 2006-10-11** (CHM; Wikipedia). Final Classic-Mac-OS-era release: 6.1.1 (2004-05-18) (Wikipedia).

**Eudora OSE / Penelope (2007–2013).** Qualcomm sponsored a Thunderbird-based open-source rewrite for a "soft landing", at times paying Dorner, Beckley, Dale Wiggins, Geoff Wenger, Matt Dudziak, Mark Charlebois. Beta 8.0 (Aug 2007) "was panned by the Eudora faithful" — different look/feel, incompatible mailbox format; OSE 1.0 (Sep 2010) "fared no better"; deprecated 2013 with advice to switch to Thunderbird. Beckley on why the clone failed: "Classic Eudora had many years of very detailed design and implementation in it... Users come to depend on those subtle interactions, and, when they aren't there, it just feels different" (CHM).

**Source release (2018).** 2018-05-22: after five years of negotiation the Computer History Museum acquired code, trademarks, copyrights, domains; published under BSD license (profanity sanitized, expired third-party components removed). Windows tree: C++, 8,651 files/565 folders, 458 MB; Mac tree: C, 1,433 files/47 folders, 69.9 MB. License excludes the trademarks (CHM).

**Afterlife (2018–2026).** CHM (updated Sept 2024): one ongoing revival — **Eudoramail 8.0 by "Hermes"**, alpha, pay-what-you-want; "well over a thousand people are still running Eudora 7". Survival kit: QCSSL/stunnel TLS workarounds, [Eudora_fix_mbx](https://github.com/LenShustek/Eudora_fix_mbx), [email-oauth2-proxy](https://github.com/simonrob/email-oauth2-proxy), the eudora-win and "Gazelle" mailing lists. Hermes history: started Aug 2018 patching the Windows code; Eudoramail dev began 2019-06-17 (Perforce Stingray UI toolkit); first 8.0 alpha 2022-08-01; current 8.0 alpha 30c ([Wikipedia](https://en.wikipedia.org/wiki/Eudora_(email_client)), rev. 2026-10-02; [hermes.cx](https://hermes.cx/)).

**Why it died technically** (verified via Wikipedia): no charset support in Windows Eudora (sent as iso-8859-1 regardless; displayed in system codepage) — fatal in the UTF-8 era; pre-Heartbleed TLS and no modern root store; IE Trident HTML rendering; the Mac version never made a clean native-OS X transition; Eudora OSE had to drop the X1 search engine (licensing).

## 3. Options compared

(coming)

## 4. Recommendations for Sweetbriar

(coming)

## 5. Risks and open questions

(coming)

## 6. Sources

(consulted URLs accumulating at end of file)

