# TODO: Plex / Trakt Integration Notes

Preserve the Plex/Trakt research here while implementation focus stays on the Apple TV / `pyatv` path.

## Current Recommendation

- Prioritize `pyatv` / Apple TV watching for **Infuse on Apple TV**.
- Keep **Plex** as the best metadata-rich fallback/parallel path.
- Treat **Trakt** as enrichment or secondary automation input unless its real-time behavior is explicitly validated in our environment.

## Why This Still Matters

- `pyatv` is attractive because it can observe **Infuse itself** on the Apple TV.
- Plex and Trakt are still valuable because they can expose richer identifiers than raw Apple TV metadata:
  - Plex can provide `guid`, `ratingKey`, `key`, player info, and structured metadata in webhook payloads.
  - Trakt/Plex paths may expose TMDB/IMDb-linked records, which makes BEQ matching cleaner than pure title string search.

## Plex: High-Value Hooks

### Primary webhook path

- Official Plex support article: `Webhooks`
  - <https://support.plex.tv/articles/115002267687-webhooks/>
- Webhooks are a **Plex Pass** feature.
- Plex posts a multipart HTTP payload to a configured URL.

### Playback events worth consuming

- `media.play`
- `media.pause`
- `media.resume`
- `media.stop`
- `media.scrobble`
- `playback.started`

### Useful payload fields

- Top level:
  - `event`
  - `user`
  - `owner`
- `Account`
  - `id`
  - `title`
- `Server`
  - `title`
  - `uuid`
- `Player`
  - `title`
  - `uuid`
  - `local`
  - `publicAddress`
- `Metadata`
  - `ratingKey`
  - `key`
  - `guid`
  - `type`
  - `title`
  - parent/grandparent fields for episodes/tracks

### PMS API endpoints to revisit

- Official PMS docs root:
  - <https://developer.plex.tv/pms/>
- Confirmed from current docs:
  - `GET /status/sessions`
- PMS docs also expose real-time event sections:
  - `Connect to Eventsource`
  - `Connect to WebSocket`
- The Redoc payload clearly contains `/:/events`.
- The WebSocket notifications path likely remains the traditional Plex notifications socket, but **verify exact path at implementation time** before coding against it.

### Why Plex is attractive

- Cleaner matching than `pyatv` string search.
- Structured metadata and player identity.
- Real-time via webhooks.
- Best path when playback is happening in the **Plex app on Apple TV**.

## Trakt: Endpoint / Timing Notes

### Official references found

- Trakt API docs root:
  - <https://trakt.docs.apiary.io/reference/notes>
- Trakt apps page:
  - <https://trakt.tv/apps>
- Trakt forum post on Plex Sync:
  - <https://forums.trakt.tv/t/plex-sync/69047>
- Firecore Trakt Sync article:
  - <https://support.firecore.com/hc/en-us/articles/4405098078743-Trakt-Sync>

### Scrobble endpoint family to revisit

These are the known Trakt scrobble endpoints we should verify against current official docs when implementing:

- `POST https://api.trakt.tv/scrobble/start`
- `POST https://api.trakt.tv/scrobble/pause`
- `POST https://api.trakt.tv/scrobble/stop`

Notes:

- The official Trakt docs describe scrobbling as the mechanism for `start`, `pause`, and `stop` playback events.
- Multiple third-party integrations and Trakt-related issue references also point to the `/scrobble/start`, `/scrobble/pause`, and `/scrobble/stop` path family.
- We should still verify required headers, payload shape, and auth details from the current official Trakt API contract before implementation.

### Infuse + Trakt findings

- Firecore says Infuse Trakt sync supports:
  - watched history
  - playback position for in-progress items
  - ratings/comments
- Requirements:
  - Trakt account linked in Infuse
  - scrobbling enabled in Infuse
- Firecore does **not** publish an exact latency SLA for when Trakt becomes queryable after playback starts.

### Plex + Trakt findings

- Trakt `Plex Sync`:
  - runs **every hour**
  - syncs watched history, library, and ratings
  - is **not** the right source for active “watching now” state
- Trakt explicitly says Plex Webhook is still the better option for “watching now” style visibility.

### Practical timing assumptions

- **Infuse -> Trakt direct scrobble**
  - likely near-live
  - expect seconds to maybe tens of seconds, not an exact guaranteed timestamp
  - validate empirically before relying on it for automatic BEQ load
- **Plex Sync -> Trakt**
  - hourly
  - not suitable for immediate auto-apply
- **Plex Webhook -> Trakt**
  - near real-time

## Matching Tradeoffs

### `pyatv` / Apple TV

- Pros:
  - can observe Infuse itself
  - local
  - does not depend on Plex
- Cons:
  - likely mostly title/year/app-level matching
  - weaker identifiers
  - requires string search and confidence scoring

### Plex

- Pros:
  - strongest metadata path
  - real-time via webhook
  - easiest BEQ match path
- Cons:
  - only helps when playback is actually visible through Plex

### Trakt

- Pros:
  - can bridge systems
  - can improve identifier quality
- Cons:
  - cloud hop
  - timing uncertainty for direct Infuse scrobble
  - hourly if using Plex Sync instead of webhook/scrobble

## Implementation TODOs

- [ ] Validate current Plex real-time endpoints beyond `GET /status/sessions`, especially the exact WebSocket notifications path.
- [ ] Capture one real Plex webhook payload from an Apple TV client and document the fields we actually receive for movies and episodes.
- [ ] Verify whether Plex webhook payloads for Apple TV playback include enough identifier data to map directly to TMDB/IMDb/BEQ entries without fuzzy matching.
- [ ] Validate the exact Trakt scrobble request/response contract for `/scrobble/start`, `/scrobble/pause`, and `/scrobble/stop`.
- [ ] Measure actual end-to-end delay for Infuse -> Trakt scrobble in a real setup:
  - note playback start time
  - poll Trakt
  - record when “watching now” or equivalent state becomes visible
- [ ] Measure actual end-to-end delay for Plex webhook -> local receiver and Plex Sync -> Trakt.
- [ ] If we revisit Plex/Trakt after `pyatv`, decide whether they should be:
  - metadata enrichment only
  - alternate trigger paths
  - or the preferred path whenever available
