# Doc 23 — Owner Interrogation & Repo Audit Findings

*Generated 2026-08-16 by reading all 34 spec/handoff docs (00–22 + handoff/ + session-bridge/) and ~55 source files across the Rust workspace, the Tauri shell, and the React overlay UI. Purpose: (1) flag what actually needs fixing/deciding right now, (2) ask Rajeev ~50 specific, mechanism-level questions the docs and code leave open, so the product direction is explicit rather than implied by whatever the code happens to do.*

**How to use this:** it's organized into named sections so you can go through it in chunks rather than one sitting — the "read first" section below is the only part that's urgent. Everything after that is grouped by subsystem; pick whichever section matches what you're thinking about, skip the rest, come back later. Nothing here needs to be answered in order.

---

## Read first — not preference questions, actual issues

These aren't "what do you want" — they're things that are broken, unverified, or a real gap between what the product promises and what the code does.

- **The core trust promise has no automated proof.** Aperture's headline claim is "zero network egress until you explicitly approve a send." The one test built to prove that at the integration level (`gates/tests/sc5_network_monitor.rs`) is 100% `todo!()`-stubbed and `#[ignore]`'d — it currently proves nothing. The claim is real at the unit-test level inside `reasoning-gateway` alone, but nothing proves some *other* crate never opens a socket. Same story for the on-hardware gates (SC3 load times, SC4 STT latency, M8 PresentMon, SC6 VRAM release) — all stubbed or `#[ignore]`'d pending real-hardware runs.
- **A decode bug meant the flagship feature produced zero suggestions until yesterday.** `Token::decode` wasn't restoring `:` from `_`, so every hydrated pattern was permanently un-bubbleable after the first restart. 528 patterns were mined and none could ever fire. Fixed 2026-08-15 — worth confirming it's actually shipping bubbles for you now, not just fixed in the working tree.
- **A 36-agent review on 2026-08-14 found 31 issues** (7 HIGH, 11 MEDIUM, 8 LOW) — things like: a SQLCipher migration crash window that silently destroys your entire history while logging "migration verified"; the "Approve for Claude" button in the MCP flow instantly cancelling its own approval (the whole gated-search release flow dead-ends); killing a sidecar host orphaning ~4.7GB of VRAM because nothing reaps the grandchild process; STT calls over ~18 seconds always failing (body-size limit). All 7 HIGH and all 11 MEDIUM are marked fixed in the working tree as of 2026-08-15. **Worth explicitly re-verifying on the installed app, not just the working tree**, since that hasn't been separately confirmed.
- **Screenshots bypass redaction entirely.** Text payloads go through a 6-rule redaction pass before you ever see the preview. Screenshots (opt-in enrichment) get zero automated scrubbing — no blur, nothing — you're relying purely on eyeballing the raw image before Send.
- **The v2 agent crates are real but the safety-critical parts are stubs.** `action-executor`'s only production backend (`UiaExecutor`) unconditionally returns "can't do that" for every action — it cannot touch your desktop today, which is good, but it means none of the guardrails (exclusion-list check, confidence-gated pausing, capability-token enforcement on who can invoke it) have been exercised for real yet. They're documented as intentions, not yet backed by code.
- **Doc set is out of sync with the code on two hard numbers.** The VRAM ceiling and idle-unload window were tightened in code (7.2GB→7.0GB, 90s→60s) citing ADRs that don't exist in the doc set you have. Both changes are conservative (safer), but worth reconciling so the docs aren't lying to future-you.

None of this should stop you from using the app — just flags where "it's built" and "it's proven" currently diverge.

---

## A. Bubble UI & lifecycle

1. The bubble stack caps at **3 simultaneous visible bubbles**, with the 4th+ queued invisibly and promoted by highest confidence when a slot frees. Is 3 right, and should freshness factor into which queued bubble gets the next open slot (currently pure confidence — a stale-but-confident suggestion can permanently outrank a brand-new one)?
2. Of those 3, only **2 render as translucent glass**; the 3rd is forced fully opaque (this is deliberate, per the design-system ADR). Keep that visual downgrade permanently, or was it only a stopgap pending the never-run GPU-load test?
3. Bubbles auto-dismiss after **20 seconds** of dwell (pausing on hover). Right default? Should it be user-adjustable from the Dashboard rather than only via a settings file?
4. "Mute this pattern" and "Exclude this app" in the bubble's overflow menu are currently **both no-ops** — they just dismiss the bubble, nothing is actually muted or excluded. Do you want these wired for real (requires the bubble to carry source-app/URL metadata), or should the affordances be pulled for v1 so they're not misleading?
5. The 👍/👎 "useful?" rating — explicitly the intended data source for measuring suggestion quality — exists in the Dashboard's Suggestions tab (retroactive, after the bubble is gone) but was never rendered on the bubble itself in the live overlay. Do you want it on the bubble too, or is retroactive rating from the Dashboard enough?
6. Each monitor runs a fully independent bubble/HUD stack with no shared dismissal state — dismissing a bubble on monitor 1 leaves an identical, clickable copy live on monitor 2 (confirmed, not theoretical — including a duplicate "Ask Claude" that can open a second preview). Is per-monitor-independent state fine for now, or does this need a single shared state before it ships more broadly?

## B. Click-through & overlay window mechanics

7. Hover-to-interactive works by publishing hit-test rectangles from React to a Rust-side poller running at ~30Hz, with 8px of padding around each rect, and the *whole window* (not per-pixel) flips click-accepting when your cursor is inside one. Has this been checked against fast mouse flicks across a bubble's edge, or is it "seems fine" pending real use?
8. During first-run consent, the overlay goes **fully modal** — it steals input and OS focus for the entire monitor until you finish the flow, so you can't interact with anything else on that screen. Intentional, or should first-run be less blocking?
9. Only the primary monitor gets the HUD, Dashboard, Privacy panel, and Context Preview — secondary monitors only get bubbles/voice surfaces. If your primary work surface is a secondary monitor, you'd have to move to the primary display to reach any control. Fine for v1, or worth fixing before wider use?

## C. Pattern engine & proactivity tuning

10. A suggestion fires after **≥3 observed returns** at confidence **≥0.7**, both hardcoded guesses pending real dogfood data. Now that the decode bug is fixed and patterns can actually fire — do these feel right on your real usage, or do they need a tuning pass before you trust the defaults?
11. Pure window-focus patterns (e.g. "you always switch to Slack after standup") can **never** produce a bubble by design — only patterns that end in one of the 4 resumable connector states (browser/video/doc/IDE) qualify. Intentional scope limit, or do you want a lighter "switch to X" affordance for non-resumable patterns too?
12. Time-of-day patterns ("opens the budget sheet ~9am") are mined and stored but **never used** to trigger anything — the code path that would check them at trigger time is never called. Wire it for v1, or is sequence-only the intended scope?
13. The pattern engine reads hardcoded constants, not the `pattern_engine` block that's already sitting in your settings file (which nothing currently reads). Do you want these genuinely user-tunable via an Advanced settings panel, or is hardcoding fine for now?
14. Two independent, uncoordinated processes prune the `patterns` table on two different 24h timers with two different rules (decay-based vs. age-based) — they can disagree with each other. Worth unifying into one source of truth, or is the current redundancy harmless enough to leave?
15. The adaptive suggestion-frequency cap (2–8/hour) only reacts to explicit clicks/dismissals/thumbs — a bubble that's simply ignored until it times out doesn't nudge the cap down at all. If most real disengagement looks like "I just let it expire," should that count too?

## D. Privacy, consent & the transparency gate

16. Given the search-oracle bug (Claude could get a match/no-match signal over your raw unredacted history with zero approval) has apparently been fixed — do you want to personally re-verify that flow before trusting it, given how much of the product's promise rests on it?
17. Screenshots ship to Claude with **zero automated redaction** (see "read first" above) — is manual eyeballing in the preview acceptable permanently, or do you want image-level scrubbing (blur likely-sensitive regions, or OCR-then-redact-then-recompose) before screenshot enrichment goes wider?
18. The secret-detection rules catch AWS keys, OpenAI-style tokens, PEM headers, and JWTs — but not GitHub/Slack tokens, generic `Authorization: Bearer` headers, or SSH private-key bodies (only the header line). Worth expanding before you trust it with real work sessions?
19. The exclusion list ships **completely empty by default** — a fresh install captures your password manager and banking apps until you manually run the onboarding suggestion flow, which itself only scans two Windows folders one level deep (so a non-default install location gets no suggestion at all). Is "empty by default, user opts into exclusions" still the right default posture, or do you want some sensible defaults baked in (password managers, banking domains) regardless of scan coverage?
20. ADR-026's "scoped always-allow" (approve once per app+intent instead of every single send) was a locked decision that was **never actually built** — its settings keys were quietly deleted, punting the whole idea to v2. Do you still want per-app scoped-allow in v1, or is "v2 only" the real final call — and if so, should that be written down explicitly somewhere rather than left as a v1 decision on paper that nothing implements?
21. Default retention: events 90 days, OCR text 30 days, voice 30 days, suggestions/patterns 180 days, audit 30 days — all guesses, never revisited. Given OCR text is the most sensitive raw-content column, should it default shorter than 30 days?
22. "Purge All" explicitly keeps your exclusion rules, consent settings, and the **last 30 days of the audit log**, despite the button being labeled "Purge all/everything." The in-panel copy discloses this — is 30 days the right retained window, or should the button's language change to match what it actually does?

## E. Voice / push-to-talk

23. STT ships as CPU-only (whisper.cpp, `base.en`) — the GPU path the build plan calls for was never integrated, and until recently the settings file was advertising a model that could never actually spawn. Worth investing in a CUDA build, or is CPU-only fine for how you actually use PTT?
24. VAD/hotkey thresholds were tuned against one dev laptop's microphone, with two earlier threshold values the code admits were wrong on real hardware. Want this validated against your actual mic setup before you rely on it daily?
25. The confirm-before-acting chip only appears below **0.6 transcription confidence**; above that, your spoken command runs immediately with no confirmation. Right cutoff, and should it be adjustable (e.g. "always confirm" mode)?
26. A PTT hold force-finalizes at **30 seconds** with no warning as you approach it. Long enough for how you'd actually use it, and do you want any signal before the cutoff?
27. "Ask Claude" from a voice answer currently builds a payload containing only the raw transcript — no screen context, no prior utterances — because the real payload builder for voice escalation is still a stub. Is that acceptable for now, or does voice-triggered Claude calls need richer context before it's useful?

## F. Vision, OCR & VLM

28. The VLM (screen-understanding escalation) only works on the original dev machine right now — model weights are manually hardlinked, there's no fetch flow for a fresh install, so any other machine silently degrades to OCR-only with no user-visible signal that VLM never activated. Want a real first-run download flow before this goes to another machine, or is dev-machine-only fine while you're the only user?
29. The VLM wake budget (documented as a hard ceiling of 10/hour) isn't actually enforced anywhere in the vision code — it's asserted to live in orchestration, unverified from this pass. Worth confirming that's real before trusting the "won't hammer the GPU" claim?
30. VLM confidence labels ("high"/"medium"/"low") get mapped to fixed numbers (0.9/0.6/0.3) because the model returns strings instead of numbers — discovered live against real output. Given the model's tendency toward non-conforming JSON, how much should the UI actually trust its structured output vs. treat it as a loose hint?
31. Binary payloads (screenshots, audio) currently ride as JSON number arrays over local HTTP between processes rather than raw bytes — a real bug already hit from this (body-size limit) was patched by raising the limit, not switching encoding, even though a more efficient path already exists elsewhere in the same codebase. Worth fixing properly, or is it not worth the effort while it's all localhost traffic?

## G. Capture & exclusions

32. Private/incognito-window detection is a hardcoded, **English-only** list of title suffixes. If you ever use a non-English browser locale, or plan to, this needs its own pass before then.
33. The window-identity cache clears itself **entirely** once it tracks 512 windows (rather than evicting the oldest), which silently drops close-events for any window opened before the last clear, in any session with heavy tab/window churn. Worth fixing to a proper LRU?

## H. Connectors & deep-link resume

34. YouTube's "position unknown" fallback is always "reopen from the start" — no heuristic like "seek back a bit from the last time we saw you watching." Is a smarter fallback worth building, or is honest "from the start" the right permanent floor?
35. How long should a paused-video state stay resumable before it's considered stale? Currently a flat 7 days for every connector type — should a document or IDE file (which arguably stays "worth resuming" much longer than a paused video) get a longer window than a video does?
36. Communication-app threads (Slack/Teams/Discord) are flagged in the docs as "the obvious next connector" once v1 ships. Still your next priority, or is there something else you'd rather have resumable first (more office apps, terminal/cwd, something else entirely)?

## I. Reasoning gateway & Claude integration

37. MCP (Claude Desktop) is the primary transport, and it's **pull-only** — Aperture can't push a prompt into Claude Desktop, it can only wait for Claude to ask. In practice, does that pull-based UX feel natural to you, or is it friction you'd rather solve by leaning on the CLI/API transport instead for your actual workflow?
38. If a payload is too large to send (one huge OCR blob or screenshot pushing it over the limit), the only current behavior is a hard error — no partial-send, no auto-truncation of the oversized item. Is a hard stop the right UX, or would you rather it try to shrink automatically (truncate text, downscale the image) and tell you what it dropped?
39. A failed write to the audit log (the one place that answers "what has ever left this machine") is currently silent — logged to a debug log only, no UI warning, and the send itself still succeeds (correctly, since the bytes already left). Should that failure surface to you somehow, given the audit trail is your only record of what went out?
40. What are the real hard size limits for each transport (CLI stdin, Messages API request size) that the payload builder should actually enforce, versus the current 50KB *soft warning* that doesn't block anything?

## J. Orchestration & GPU/VRAM budget

41. The projected-VRAM ceiling (7.0GB) and idle-unload window (60s) are both fixed values, not user-configurable, regardless of what GPU is actually installed. If you ever run this on a different machine (more or less VRAM than the 5060), do you want these to auto-scale, or is "built for this exact hardware" an acceptable permanent constraint?
42. When a request is refused for exceeding the budget after the full degrade ladder, the only behavior is refuse-and-notify — never queue-and-retry. Is immediate refusal the right UX for something you weren't in a hurry for, or would a "wait a moment" option be worth it for non-realtime requests?
43. One shared lock currently guards both the VLM and the STT model slots, so a slow VLM cold-load (up to the 15-60s window depending on config) can stall an unrelated incoming voice request even though voice is supposed to never be starved. Worth splitting the lock, or is this a real-world-rare enough case to leave?
44. The seeded VRAM estimate for the Whisper co-resident model is double what the original budget doc assumed (~2GB measured vs ~1GB documented) — meaning the advertised "VLM + Whisper both resident, no swapping" default may collapse into forced swapping the moment an image job runs. Worth a real measurement pass before you rely on "no swap needed" being true day-to-day?

## K. The v2 agent execution layer (the biggest open area)

This is genuinely the least-settled part of the whole project — it's real code (task state machine, audit trail) wrapped around a deliberately inert "hands" component that can't touch your desktop yet. The questions below are the ones the docs themselves list as blocking before any of it gets wired up for real.

45. Once wired up, should every single agent action (click/type/launch/etc.) require your live confirmation, or should routine low-risk actions run without asking while anything that looks destructive (delete, send, purchase, overwrite) always stops for confirmation regardless of Claude's self-reported confidence?
46. v1's whole trust story is "you see the exact bytes before anything sends, every time." The v2 design as documented has you approve a *task* once, then Claude gets a stream of screenshots per step without a forced preview per step (full payload viewable on demand, not forced). Is that an acceptable evolution of the v1 promise for you personally, or does it need to feel more like v1 — a real preview at some cadence, not just "available if you go looking"?
47. When the agent hits an app on your exclusion list mid-task, should it pause and tell you, or silently skip that step and keep going? (This blocks the next build milestone and is completely undecided right now.)
48. For UAC-elevated windows (Task Manager, installers, anything asking "Run as administrator") — should the agent hard-refuse those entirely, or offer a "run as admin" prompt (which would require giving the executor more privilege than it has today)?
49. What's your real risk tolerance for a wrong click? The primary way the agent finds things to click is fuzzy text-label matching (allows for typos/near-misses) with a fallback to raw pixel coordinates if that fails — reasonable for "click the blue Continue button," riskier for anything near a delete/send/pay action. Do you want a stricter, allow-list-style check specifically for actions that look consequential, on top of Claude's own confidence signal?
50. Should the agent ever be allowed to touch the filesystem, run shell commands, or make network calls directly, or is "screen and keyboard only, forever" a line you want held hard regardless of how useful an exception might seem later? (The code currently treats this as a hard invariant, not a temporary v2.0 limitation.)
51. What should the user-facing UI actually look like while a multi-step task is running — a persistent visible status bar so you always know Aperture is driving your mouse/keyboard right now, a live step-by-step log you can scroll through, or something else? And when the agent gets stuck and needs your input mid-task, does it wait indefinitely or time out?
52. Is there any "undo" story for actions already taken before you hit the kill switch (a partially-filled form, an app that got launched), or is a hard stop purely "no further steps" with no attempt to reverse what already happened?

## L. Design system (Chromemorphism / Liquid Meta)

53. The blur ceiling was already cut from 16px to 12px, and glass fills are near-opaque (α ≥ 0.96) specifically because more translucent versions were illegible on busy backgrounds in real testing. Given what's shipping is closer to "mostly-opaque dark panels with soft edges" than true glass/translucency, are you happy with how it actually looks now, or is there appetite to push translucency further even at some legibility cost?
54. "Liquid" refraction/distortion effects were deferred out of v1 entirely (static glass only, for now). Still want that in a future pass, or has the static version grown on you enough that it's not worth the GPU cost to add later?

---

**A note on how to answer this:** you don't need to go in order or answer everything in one sitting — pick a section, tell me your calls, and I'll fold them back into the docs/settings/code as we go. Anything you skip just stays as-is (whatever the current default/behavior already is).
