# Doc 08 — Behavior & Pattern Engine

## 1. Interface
| | |
|---|---|
| **Inputs** | Event stream (bus + SQLite), embeddings (`ctx_vec`), fresh `connector_state`, suggestion feedback events |
| **Outputs** | `SuggestionCandidate{action_template, connector_id, confidence, pattern_id}` → Suggestion Generator → Bubble UI. Optional VLM disambiguation requests (Doc 06 §4a). **Never a cloud call** (locked answer A) |
| **Resource cost** | CPU-only, incremental; O(recent-window) per event; negligible RAM beyond pattern table cache |

## 2. Event normalization → tokens
Each event becomes a token `(app_class, action, resource_class)`:
- `app_class`: process mapped through a small alias table (chrome/edge/firefox→`browser`, code→`ide`, excel/winword→`office`, else process name).
- `action`: the event type (focus/open/navigation/media/document/ide).
- `resource_class`: from the connector type when present (`youtube`, `doc:xlsx`, `ide:rs`, `url:domain`), else `∅`.
Example: opening a tutorial video ⇒ `(browser, navigation, youtube)`.

## 3. Sessionization
A new `session_id` starts after **15 min** of no input activity [ASSUMPTION]. Sessions bound n-gram extraction so overnight gaps don't fabricate sequences.

## 4. Signatures & statistics
- Extract sliding **n-grams (n = 2..4)** of tokens within a session. The trailing token is the *consequent*; the prefix is the *antecedent*.
- `signature = join(antecedent) ⇒ consequent` stored in `patterns`.
- **Support:** count of occurrences, recency-weighted: each occurrence contributes `w = 0.5^(age_days/7)` (exponential half-life **7 days** [ASSUMPTION]).
- **Confidence:** `conf = W(antecedent ⇒ consequent) / W(antecedent ⇒ *)` — a recency-weighted conditional probability.
- **Temporal patterns:** independently, per-resource return-visit periodicity (time-of-day bucket histogram, 2-hour buckets). A resource with ≥3 weighted returns in the same bucket forms a `temporal` pattern (e.g., "opens the budget sheet ~9am").

## 5. Candidate generation & scoring
On each new event, match the current token tail against pattern antecedents (exact match on n-grams; plus a semantic assist: cosine similarity of the current context embedding to the pattern's stored centroid ≥ 0.75 may substitute for one token [ASSUMPTION — evaluate at M3]).
```
score = conf                       // §4
      × dismiss_decay              // §7 feedback
      × freshness(connector_state) // 1.0 if within TTL, else 0 → no candidate
      × novelty                    // 0 if the resource is ALREADY focused (never suggest what's on screen)
```

## 6. Proactive trigger rule (all must hold)
1. `score ≥ τ_conf = 0.6` [VERIFY — tuned against SC7 at M3]
2. Weighted support ≥ **3** (cold-start floor [ASSUMPTION])
3. A **fresh, resumable** `connector_state` exists for the consequent (Doc 10 TTLs)
4. Cooldown: same signature not shown in the last **30 min** [ASSUMPTION]
5. Global cap: ≤ **4 suggestions/hour** [ASSUMPTION]; queue overflow drops lowest score
6. The consequent's resource is not currently foreground (novelty)
7. Capture is ON
When all hold ⇒ emit the candidate; the Suggestion Generator renders `action_template` ("Continue {title} — {position}") into a `BubbleSpec`.

## 7. Feedback loop
| Signal | Effect |
|---|---|
| `suggestion_clicked` | `dismiss_decay ← min(1.0, decay × 1.25)`; support reinforced |
| `suggestion_dismissed` | `dismiss_decay ← decay × 0.5`; two dismissals in 24 h ⇒ signature muted 7 days [ASSUMPTION] |
| `suggestion_expired` (ignored) | mild: `decay × 0.9` |
These write back to `patterns` and are the lever that meets SC7 without cloud help.

## 8. VLM assist (optional, local)
When a high-support antecedent matches but `resource_class = ∅` (OCR couldn't classify the consequent), the engine may file a Doc 06 wake request to classify the scene. The candidate is **not** delayed for it (Doc 02 Path A invariant).

## 9. Failure modes
| Failure | Behavior |
|---|---|
| Cold start (new install) | No suggestions until floors are met — silence beats noise |
| Over-triggering | Caps/cooldowns + decay (§6–7); tunables exposed in settings |
| Concept drift (habits change) | Half-life weighting ages old patterns out in ~3 weeks |
| Pattern table bloat | Prune signatures with weighted support < 0.5 weekly |
| Clock changes / DST | Temporal buckets keyed to local wall-clock by design |
