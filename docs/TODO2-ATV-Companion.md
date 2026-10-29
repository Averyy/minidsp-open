# Apple TV Companion Watcher Research

This note is a focused research document for the `minidsp-open` Apple TV idea:

- watch one specific Apple TV on the LAN
- determine what it is playing in real time
- match that title to ezBEQ catalog data
- suggest the detected movie and let the user choose the matching BEQ variant

This is intentionally not a generic `pyatv` note. The question here is narrower:

- what Apple officially exposes
- what Apple clearly uses internally
- what reverse-engineered implementations prove
- whether passive traffic snooping is enough
- what a practical "view-only" watcher should look like

## Executive Findings

- Apple officially ships Apple TV remote surfaces on iPhone, iPad, and Apple Watch, and Apple documents the surrounding user flows, discovery model, and security model.
- Apple publicly documents app-side Now Playing publishing APIs and generic local-network discovery and security primitives, but I did not find a public Apple API that lets a third-party app observe what an Apple TV is currently playing over the LAN.
- Apple-origin private SDK artifacts in Xcode show that Apple has an internal now-playing and system-status client stack built around `TVRemoteCore`, `MediaRemote`, and `Rapport`.
- Multiple non-`pyatv` projects confirm that modern Apple TV observation/control runs through Companion Link with pairing, pair-verify, and encrypted sessions.
- Passive same-LAN packet capture is not enough to recover useful playback metadata after pairing. Reverse engineering can still observe traffic, but only through an active participating client or proxy that has the session keys.
- For `minidsp-open`, the cleanest product path is a paired, read-only LAN-side watcher. It does not need to implement full remote-control parity to be useful.

## Direct Answers

### Why Pairing Is Required

Pairing is required because Companion is not an anonymous local status feed. It is a trusted client session.

In practice, pairing provides:

- authorization, so the Apple TV accepts the client as a legitimate remote-side participant
- credential material for later reconnects
- the basis for pair-verify and encrypted session setup

Same-LAN access is enough for discovery. It is not enough for useful playback observation.

### What Companion Is Normally Used For

Companion appears to be Apple’s modern local Apple TV remote/client channel.

Based on Apple private-framework symbols and reverse-engineered implementations, that channel is used for things such as:

- remote sessions from iPhone, iPad, and Apple Watch
- now-playing and up-next fetches
- system-status and attention-state monitoring
- app and media actions
- button, touch, and text-input sessions

For `minidsp-open`, we only care about the observer slice of that stack.

### Other Direct Ways To Get Now-Playing Data Off Apple TV

I did not find a second clean public LAN observer API besides the same Apple remote stack that Companion belongs to.

The closest alternatives are:

- Apple private-framework clients using `TVRemoteCore`, `MediaRemote`, and `Rapport`
- older historical remote protocols such as MRP, which are useful context but not the primary modern target
- active proxy / MITM tooling for reverse engineering, which is a method for observing the traffic, not a separate product interface

There are two narrow no-pairing edge cases worth calling out:

- legacy `DMAP` / `Home Sharing` behavior on older Apple TV eras, where discovery can expose credentials and make explicit pairing optional
- AirPlay Bonjour state changes that can reveal coarse playback state without revealing the current title or rich now-playing metadata

### Feasibility Summary

A LAN-side paired watcher is realistic.

Why:

- Apple’s own private frameworks expose explicit now-playing and system-status surfaces.
- Multiple independent non-`pyatv` projects already prove discovery, pairing, encrypted session setup, and Companion messaging.
- Our product needs only the read-only observer subset, not full remote parity.

The most realistic implementation path is:

1. build the minimal paired watcher off-device first
2. validate metadata quality and matching against real Apple TV playback
3. then decide whether a narrower embedded port is worth doing later

### Final No-Pairing Conclusion

This final research pass did not find a modern tvOS 26+ path to real now-playing metadata without a paired client.

What I did find:

- `DMAP` has an older `Home Sharing` mode where pairing can be optional because credentials may be exposed via Zeroconf properties. This is legacy behavior and not the modern Apple TV path.
- AirPlay / Bonjour advertisements can expose coarse state changes. Unofficial AirPlay protocol notes show examples where TXT-record flags differ between `not playing`, `playing local media`, and `receiving AirPlay`. That is useful for very rough activity detection, but it does not expose the movie title or rich playback metadata.

So the practical answer remains:

- no unpaired path was found for rich current-title metadata on modern Apple TV
- only legacy or coarse-state exceptions were found

## Scope and Product Goal

The product requirement is modest:

- identify the Apple TV to watch
- know what title is playing
- know enough metadata to match the title reliably at the movie level
- surface likely BEQ variants for manual selection

The product does not need:

- touch gestures
- keyboard entry
- Siri transport
- remote-control parity
- media playback initiation
- AirPlay streaming

That distinction matters because most public discussion around Apple TV protocols is control-heavy. Our use case is narrower and easier: a paired observer.

## Official Apple Evidence

### Apple’s User-Facing Remote Product Surface

Apple clearly ships and supports remote control of Apple TV from other Apple devices:

- Apple Support: `Set up the Apple TV Remote on your iPhone or iPad`
  - <https://support.apple.com/en-ca/108778>
  - Establishes that the remote is an official Apple feature.
  - States the iPhone or iPad must be on the same Wi-Fi network as the Apple TV.
  - Documents a four-digit passcode flow during setup.
- Apple Support: `Use your iPhone, iPad or Apple Watch as a remote for Apple TV 4K`
  - <https://support.apple.com/en-ca/guide/tv/atvb701cadc1/26/tvos/26>
  - Confirms Apple Watch is also an official remote surface.
  - Confirms Apple TV has a `Remote App and Devices` settings area.
- Apple Support: `Control Video Playback on Apple TV 4K`
  - <https://support.apple.com/en-ca/guide/tv/atvb7944597f/26/tvos/26>
  - Documents playback controls and `InSight`.
  - States that `InSight` information also appears on iPhone or iPad when those devices are being used as the Apple TV remote.

What this proves:

- Apple has an official remote experience that can display active playback metadata from Apple TV on other Apple devices.
- Apple TV playback metadata absolutely exists as a first-class concept in Apple’s own product behavior.

What this does not prove:

- It does not give us a public third-party API to read that data ourselves.

### Apple’s Public Developer Surface

Apple publicly documents the surrounding platform pieces:

- Apple Developer: `Bonjour`
  - <https://developer.apple.com/bonjour/>
  - Documents Apple’s local-network discovery model.
- Apple Developer archive: `Bonjour API Architecture`
  - <https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/NetServices/Articles/programming.html>
  - Documents service discovery and resolution concepts.
- Apple Developer: `Supporting remote interactions in tvOS`
  - <https://developer.apple.com/documentation/avfoundation/supporting-remote-interactions-in-tvos>
  - Public app-side guidance around remote interactions.
- WWDC 2017: `Now Playing and Remote Commands on tvOS`
  - <https://developer.apple.com/videos/play/wwdc2017/251/>
  - Explains how apps publish Now Playing metadata and respond to remote commands.
  - Explicitly says the TV Remote app for iOS displays metadata and playback controls.
- WWDC 2022: `Explore media metadata publishing and playback interactions`
  - <https://developer.apple.com/videos/play/wwdc2022/110338/>
  - Explains `MPNowPlayingSession`, `MPNowPlayingInfoCenter`, and `MPRemoteCommandCenter`.
  - States that the Apple Watch Now Playing app even has an Apple TV remote built in.

The common thread in these public Apple sources:

- Apple documents how apps publish Now Playing metadata.
- Apple documents how the system and other devices can present that metadata.
- Apple does not document a public API for a third-party LAN observer to ask an Apple TV what it is playing.

That last sentence is a research conclusion from surveying Apple’s public docs. It is not phrased by Apple directly, but I did not find a public Apple document exposing such an API.

### Apple’s Published Security Model Around Apple TV-Related Accessory Traffic

Apple Platform Security gives the clearest official security evidence:

- Apple Platform Security: `HomeKit Communication Security`
  - <https://support.apple.com/en-ca/guide/security/sec3a881ccb1/web>
  - Describes Ed25519 identities, HAP-authenticated direct communication, and encrypted sessions.
- Apple Platform Security: `HomeKit Security with Apple TV`
  - <https://support.apple.com/en-ca/guide/security/sec5d283419e/web>
  - Describes third-party remote accessories sending HID events and Siri audio to Apple TV over secure sessions.
  - Explicitly states that a per-session `HKDF-SHA512`-derived key pair encrypts the local network connection.

Why this matters for us:

- Apple’s own published security material supports the conclusion that Apple TV-adjacent remote traffic is authenticated and encrypted on the local network.
- That lines up with what the reverse-engineered Companion implementations show.

### Apple’s Official Packet-Capture Guidance

Apple also publishes generic packet-trace guidance:

- Apple Developer: `Recording a Packet Trace`
  - <https://developer.apple.com/documentation/network/recording-a-packet-trace>
- Apple Developer: `Troubleshooting Packet Traces`
  - <https://developer.apple.com/documentation/network/troubleshooting-packet-traces>

These are useful for capture mechanics, but they do not provide a Companion-specific decryption or playback-observer path.

## Apple-Origin Private SDK Evidence

The strongest Apple-side evidence is in Apple’s own shipped private frameworks.

### SDK Paths Inspected

- `/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX26.2.sdk/System/Library/PrivateFrameworks/TVRemoteCore.framework/Versions/A/TVRemoteCore.tbd`
- `/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX26.2.sdk/System/Library/PrivateFrameworks/MediaRemote.framework/Versions/A/MediaRemote.tbd`
- `/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX26.2.sdk/System/Library/PrivateFrameworks/Rapport.framework/Versions/A/Rapport.tbd`

### `TVRemoteCore` Symbols

Apple’s `TVRemoteCore` exports symbols directly relevant to our use case:

- `TVRCFetchAttentionState`
- `TVRCFetchCurrentNowPlayingInfoEvent`
- `TVRCFetchUpNextInfoEvent`
- `TVRCNowPlayingInfoEvent`
- `TVRCSystemStatus`
- `TVRCNowPlayingInfo`
- `TVRCNowPlayingMetadata`
- `TVRCPINEntryAttributes`
- `TVRCRPCompanionLinkClientWrapper`
- `TVRCRapportDeviceImpl`
- `TVRCRapportDeviceQuery`
- `TVRCRapportMediaEventsManager`

This is not generic or accidental naming. Apple’s own internal framework has explicit now-playing fetch and event surfaces for the Apple TV remote stack.

### `MediaRemote` Symbols

Apple’s `MediaRemote` exports now-playing payload model symbols such as:

- `MRRapportTransport`
- `MRNowPlayingClientProtobuf._bundleIdentifier`
- `MRNowPlayingInfoProtobuf._title`
- `MRNowPlayingInfoProtobuf._artist`
- `MRNowPlayingInfoProtobuf._album`
- `MRNowPlayingInfoProtobuf._duration`
- `MRNowPlayingInfoProtobuf._elapsedTime`
- `MRNowPlayingInfoProtobuf._playbackRate`
- `MRNowPlayingInfoProtobuf._timestamp`
- `MRNowPlayingInfoProtobuf._artworkDataDigest`

This strongly suggests a layered internal model:

- `Rapport` as transport and device/session plumbing
- `TVRemoteCore` as Apple TV remote client behavior
- `MediaRemote` as playback and now-playing data model

### Runtime Header Evidence

Public GitHub dumps of Apple private runtime headers make the picture even clearer.

#### `TVRCRPCompanionLinkClientWrapper`

Raw header:

- <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/PrivateFrameworks/TVRemoteCore.framework/TVRCRPCompanionLinkClientWrapper.h>

Relevant members and methods:

- `TVRCRapportMediaEventsManager * _mediaManager`
- `TVRCNowPlayingInfo * _nowPlayingInfo`
- `_fetchTVSystemStatusAndStartMonitoring`
- `_startMonitoringNowPlayingInfo`
- `_startMonitoringTVSystemStatus`
- `registerEvent:options:handler:`
- `sendEvent:options:response:`
- `fetchUpNextInfoWithPaginationToken:completion:`
- `markAsWatchedWithMediaIdentifier:completion:`
- `playItem:completion:`

This is the clearest Apple-origin evidence that Apple has an internal Companion/Rapport client wrapper that can monitor now-playing and system status.

#### `TVRCDevice` and `TVRCDeviceDelegate`

Raw headers:

- <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/PrivateFrameworks/TVRemoteCore.framework/TVRCDevice.h>
- <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/protocols/TVRCDeviceDelegate.h>

Relevant surface:

- `@property (nonatomic, readonly) TVRCNowPlayingInfo *nowPlayingInfo;`
- `fetchUpNextInfoWithPaginationToken:completion:`
- `markAsWatchedWithMediaIdentifier:completion:`
- `playItem:completion:`
- delegate callback `device:updatedNowPlayingInfo:`

That delegate callback is especially important. It shows Apple has a first-class "now playing changed" update model in its own client stack.

#### `TVRCNowPlayingMetadata`

Raw header:

- <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/PrivateFrameworks/TVRemoteCore.framework/TVRCNowPlayingMetadata.h>

Relevant fields:

- `bundleID`
- `canonicalID`
- `duration`
- `episodeNumber`
- `episodeTitle`
- `extendedDescription`
- `genre`
- `iTunesStoreIdentifier`
- `mainContentStartTime`
- `productPageURL`
- `programID`
- `releaseDate`
- `seasonNumber`
- `showID`
- `showProductPageURL`
- `timeOffset`
- `timestamp`
- `title`

For our matching problem, this is excellent news. Apple’s own internal metadata object contains exactly the kind of identifiers and title data we would want for reliable movie and episode matching.

#### `TVRCRapportMediaEventsManager`

Raw header:

- <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/PrivateFrameworks/TVRemoteCore.framework/TVRCRapportMediaEventsManager.h>

Relevant clues:

- `RPMediaControlSession * _mediaSession`
- `sendMediaEvent:`
- `supportedMediaCommands`
- `volumeSupported`

This is more control-oriented, but it confirms that Rapport-backed media handling is a real subsystem in Apple’s stack.

#### `RPCompanionLinkDevice` and `MRRapportTransport`

Raw headers:

- <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/PrivateFrameworks/Rapport.framework/RPCompanionLinkDevice.h>
- <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/PrivateFrameworks/MediaRemote.framework/MRRapportTransport.h>

Relevant clues:

- `RPCompanionLinkDevice` contains pairing and device/session-oriented properties.
- `MRRapportTransport` has `RPCompanionLinkDevice *targetDevice`.

Interpretation:

- `Companion Link` is not just a pyatv abstraction. Apple’s own private frameworks represent it as a device and transport layer.

## Reverse-Engineered Evidence Beyond `pyatv`

`pyatv` is not the only source. The same basic picture appears in other work.

### `basmilius/apple-protocols`

Repository:

- <https://github.com/basmilius/apple-protocols>

Relevant files:

- `packages/companion-link/src/messages.ts`
- `packages/companion-link/src/pairing.ts`
- `packages/companion-link/src/protocol.ts`

What it contributes:

- independent Companion Link implementation in TypeScript
- explicit pairing support
- explicit session and event support
- support for `fetchNowPlayingInfo()`
- support for `fetchUpNext()`
- state observation around now playing

Important protocol details visible in code and docs:

- `_systemInfo`
- `_sessionStart`
- `_interest`
- `FetchCurrentNowPlayingInfoEvent`
- `FetchUpNextInfoEvent`

That is extremely close to the minimal watcher we would want.

### `LaurentVoillot/AppleTVr`

Repository:

- <https://github.com/LaurentVoillot/AppleTVr>

Relevant files:

- `Sources/AppleTVr/Protocol/ATVDiscovery.swift`
- `Sources/AppleTVr/Protocol/CompanionConnection.swift`
- `Sources/AppleTVr/Protocol/SRPClient.swift`
- `Sources/AppleTVr/Protocol/OPACK.swift`

What it contributes:

- native macOS client built around Companion Link
- discovery via Bonjour
- pairing and credentials
- explicit crypto flow with SRP and ChaCha20-Poly1305
- real-world modern tvOS Companion usage outside `pyatv`

This is valuable because it shows the concept is not tied to Python or the rest of `pyatv`’s feature breadth.

### `mediaremotetv-protocol`

Repository:

- <https://github.com/jeanregisser/mediaremotetv-protocol>

This is more useful as historical context for older MRP-era behavior than as the primary path for a modern tvOS 26+ watcher, but it helps confirm that Apple TV remote metadata/control has long existed as a protocol surface.

### `pyatv` Still Matters, But It Is Not the Only Evidence

Key `pyatv` references:

- Protocol docs
  - <https://pyatv.dev/documentation/protocols/#companion-link>
- Proxy docs
  - <https://pyatv.dev/documentation/atvproxy/>
- Issue threads with newer tvOS observations
  - <https://github.com/postlund/pyatv/issues/2325>
  - <https://github.com/postlund/pyatv/issues/2461>
  - <https://github.com/postlund/pyatv/issues/2656>
  - <https://github.com/postlund/pyatv/issues/2837>

Useful facts from those sources:

- Companion uses `_companion-link._tcp.local.`
- traffic is framed with a one-byte frame type plus three-byte length
- pair-setup and pair-verify exist as explicit phases
- post-verify traffic is encrypted
- Companion has event subscription via `_interest`
- `atvproxy` can log fully decrypted Companion traffic because it is an active proxy, not a passive sniffer

## What Pairing Is For

Pairing is not just a UI inconvenience. It is the mechanism that authorizes the client and establishes the cryptographic relationship used afterward.

Facts supporting that:

- Apple’s own user-facing remote setup includes a four-digit passcode flow.
- Apple Platform Security describes authenticated, encrypted local sessions around Apple TV-related remote behavior.
- The reverse-engineered Companion implementations treat pair-setup and pair-verify as core protocol phases.
- `pyatv` and other clients store credentials for reconnect.

Practical reading:

- same LAN is enough for discovery
- same LAN is not enough for useful playback observation
- pairing is what turns a discovered device into an authenticated client session

## Does Apple Home Enrollment Matter?

I did not find evidence that a separate Home app enrollment step is required for the kind of watcher we want.

What the evidence supports:

- local discovery happens via Bonjour
- pairing and verification are required
- traffic is then encrypted

So the correct statement is:

- no evidence was found that Apple Home enrollment is required for a LAN-side watcher
- strong evidence was found that pairing and credential storage are required

## Traffic Observation and Reverse-Engineering Options

This is the central answer to the "can we snoop/view the traffic?" question.

| Mode | What it requires | What you can see | Practical verdict |
| --- | --- | --- | --- |
| Passive LAN capture | Same network and a packet capture tool | Bonjour discovery, TCP endpoints, cleartext frame header, timing, lengths | Not enough for now-playing payloads after pair-verify |
| Active paired observer | Pair once, store credentials, reconnect as a client | Decrypted fetch responses, subscribed events, system status, now-playing metadata | Best path for a view-only watcher |
| Active MITM / proxy | A proxy that terminates both sides of the session | Decrypted messages because the proxy participates in both encrypted legs | Best path for reverse engineering traffic details |
| Apple-private-framework client | macOS or iOS code using private Apple frameworks | Apple-shaped now-playing objects and callbacks | Strong RE / tooling path, but private-API-only |

### What Passive Capture Can Still Tell Us

Passive capture is not useless, but it is limited:

- it can show `_companion-link` discovery
- it can show which host/port a device is using
- it can show frame sizes and timing
- it can help correlate user actions to traffic bursts

What it cannot reliably give us:

- readable now-playing titles
- bundle IDs
- program IDs
- any other useful payload once the encrypted session is established

### Why `atvproxy` Works

`pyatv`'s `atvproxy` is important because it proves a point:

- <https://pyatv.dev/documentation/atvproxy/>

It can output fully decrypted Companion messages because it is not just observing traffic. It is actively terminating one connection and creating another, so it has the necessary key material on both sides.

That means:

- reverse engineering the protocol is feasible
- passive Wireshark-only observation is not the right model

## Implications for a Read-Only Watcher

The good news is that a read-only watcher is smaller than a full remote implementation.

The likely minimal capability set is:

- discover a configured Apple TV using `_companion-link._tcp.local.`
- pair once and store credentials
- perform pair-verify and establish an encrypted session
- send `_systemInfo`
- start a session
- register interest in the relevant events
- subscribe to now-playing and system-status updates
- optionally trigger `FetchCurrentNowPlayingInfoEvent`
- optionally trigger `FetchUpNextInfoEvent`
- normalize the returned metadata
- prove that the metadata can be matched against ezBEQ reliably
- only later surface the movie match and manual BEQ choices in the main product

The watcher does not need, at least initially:

- HID button injection
- touch gestures
- text input
- Siri audio
- app launch
- watch-state mutation

## Fields Most Relevant to ezBEQ Matching

Apple-origin metadata surfaces suggest these are the most useful fields:

- title
- episode title
- season number
- episode number
- duration
- playback state
- playback rate
- elapsed time
- timestamp
- bundle ID
- canonical ID
- iTunes Store identifier
- program ID
- product page URL
- show ID
- artwork digest or artwork URL template

For movie matching specifically, the most important cluster is probably:

- title
- duration
- bundle ID
- canonical/program/store identifiers if present

This is much better than a pure string-only guesser.

## tvOS 26+ Practical Notes

Focusing on tvOS 26+ helps:

- we do not need to optimize for older Apple TV generations or older protocol eras
- Companion is clearly the relevant modern path
- we can ignore legacy protocol compatibility as a product requirement

The open reverse-engineering threads still matter because they expose current integration issues:

- newer tvOS builds appear sensitive to `_systemInfo` identity details
- there is at least one report of pairing quirks on tvOS 26.3

That means implementation should plan for:

- stable client identity
- stored credentials
- reconnect and re-pair UX
- logging during bring-up on real hardware

## Implementation Sequencing

Do not start on ESP32.

Start with a disposable desktop-native spike first.

Why:

- desktop gives much better logging and debugging during protocol bring-up
- desktop keeps credential storage and reconnect UX simpler
- desktop iteration is much faster than ESP32 flashing / tracing cycles
- the Companion protocol slice itself is portable enough to prove on desktop before worrying about embedded constraints

The first implementation should therefore be:

- isolated from the main product path
- easy to delete if pairing or metadata quality turns out to be too weak
- focused only on proving the protocol slice and the matching problem
- implemented as portable desktop code, not a macOS-only product path

The phase order should be:

1. desktop-native spike: discovery, pairing, reconnect, metadata logging
2. real-world validation on Infuse and Plex playback
3. prove that the normalized metadata matches the ezBEQ catalogue well enough
4. only then fold stable pieces into the main daemon/app architecture
5. only after that evaluate an ESP32 port

## Reference Implementations

Do not force a single-source implementation strategy here.

Use all three open-source Companion references and take the best parts of each:

- `apple-protocols`
  - primary reference for the exact metadata-focused Companion slice we care about
  - especially useful for `_systemInfo`, session start, event registration, `FetchCurrentNowPlayingInfoEvent`, `FetchUpNextInfoEvent`, and now-playing state flow
- `pyatv`
  - primary reference for overall maturity, pairing/reconnect behavior, storage ideas, and broader protocol understanding
  - especially useful when the other projects disagree or omit edge-case handling
- `AppleTVr`
  - compact native reference for a smaller end-to-end Companion client
  - especially useful for sanity-checking framing, SRP, and encrypted session lifecycle in a non-Python codebase

So the intended reading order is:

- follow `apple-protocols` first for metadata/event behavior
- cross-check with `pyatv` for pairing, verify, and robustness expectations
- use `AppleTVr` as a smaller native reference when validating the end-to-end lifecycle

This is still a clean-room implementation.

That means:

- use these projects as behavioral and structural references
- do not copy code directly
- prefer confirming message flow and state transitions over mirroring source layout

## What Might Become Shared Later

Some code may become shared later, but that should be an extraction step after the spike works.

Likely shared later:

- normalized now-playing data model
- ezBEQ matching and confidence scoring
- policy logic such as suggest vs auto-apply
- configuration schema for watched device, slot role, and matching thresholds

Do not assume these parts are shared early:

- Companion discovery and socket handling
- pair-setup and pair-verify flow
- credential storage
- reconnect lifecycle
- logging and reverse-engineering helpers

So the correct stance is:

- desktop first
- prove the risky parts first
- extract only the stable pieces later
- prefer a portable desktop implementation over a macOS-only implementation

## Adjacent Product Paths

These are not the primary Apple TV watcher path, but they matter as alternatives or fallbacks.

### Infuse

Official sources:

- `API for Third-Party Apps & Services`
  - <https://support.firecore.com/hc/en-us/articles/215090997-API-for-Third-Party-Apps-Services>
- `Media Server Sync`
  - <https://support.firecore.com/hc/en-us/articles/4405110679447-Media-Server-Sync>

What Infuse officially exposes:

- x-callback playback URLs
- deep links by TMDB ID
- media-server sync behavior

What I did not find:

- a public Infuse API for "what is currently playing right now on Apple TV"

So Infuse is useful for launch/deep-link directions, but it does not replace an Apple TV watcher for live observation.

### Plex

Official source:

- `Webhooks`
  - <https://support.plex.tv/articles/115002267687-webhooks/>

Plex webhooks provide:

- `media.play`
- `media.pause`
- `media.resume`
- `media.stop`
- `media.scrobble`
- `Player` data
- `Metadata` data including title and IDs

If playback is coming from Plex and a Plex server is available, this is the cleanest metadata source. But it solves a different problem than "watch the Apple TV itself regardless of source app."

### Trakt

Official API root:

- <https://trakt.docs.apiary.io/>

Trakt is relevant as a cloud-side watch-state feed, but it is not the same thing as a live local-device observer. It may be useful later for enrichment or fallback, but it is not the primary answer to the Apple TV LAN watcher problem.

## Recommended Product Shape

For `minidsp-open`, the most defensible path is:

- keep the Apple TV watcher as a LAN-side companion process
- keep the first implementation as an isolated, disposable desktop-native spike
- keep it focused on view-only playback observation and metadata logging
- prove pair/reconnect reliability and metadata quality before deeper integration
- prove ezBEQ matching quality before any auto-apply path

Possible implementation shapes:

- a small Rust companion crate or tool kept separate from the main daemon path
- an optional macOS-only private-framework research tool for protocol capture and field discovery if desktop-native bring-up gets stuck
- later, a more portable Companion client once the minimal protocol slice is understood well

## Repo Placement

The research and early spike should stay in this repo initially, but the implementation should remain isolated until it is proven.

Reasons:

- it is a direct product feature for `minidsp-open`
- it needs to share configuration and matching behavior with ezBEQ
- it should evolve next to the API surfaces it will call
- keeping research notes, captures, and implementation together will make iteration faster
- if the spike fails, it should be trivial to delete without touching the core daemon, protocol, or ESP32 paths

Practical implication:

- keep the first implementation in a separate crate or tool
- do not wire it into `protocol`, `daemon`, or `app-esp32` yet
- do not assume desktop and ESP32 code will be identical
- keep phase-0 state local to the repo so the experiment is easy to inspect and delete

A separate repo only makes sense later if we deliberately extract a generic reusable Apple TV watcher library.

## Current spike status

The desktop spike in [atv-companion-spike](../atv-companion-spike/) implements
discovery, pairing, restricted credential storage keyed by stable device ID,
IPv4/IPv6 reconnect hints, decoded asynchronous events, and replayable movie
suggestions. Completed changes are in [CHANGELOG.md](../CHANGELOG.md).

Real Apple TV checks established:

- Infuse and Plex provide title/duration, but no reliable app/program identity or progress. Per-item UUIDs aren't app routing keys.
- Prime supplies app/content identifiers but lacks program/show/episode fields in the captured movie sample; Apple TV movie payloads also supply program/store identifiers and progress.
- Netflix supplies empty now-playing metadata even after attention-state/bootstrap checks. Up Next can be stale or fail, so it isn't a reliable third-party playback source.
- Useful metadata arrives in later events rather than the almost-empty direct fetch reply. Address churn must be handled without re-pairing.

The matcher uses normalized title plus duration within ±15 min and exposes
candidate variants. It doesn't determine the actual audio mix; manual variant
selection remains required even for one hit. Backend/session rules, sustained
reconnect/watch behavior, ESP32 feasibility, and the product phases below are
still open. Samples and replay output stay under ignored
`.scratch/atv-companion/`.

## Approved First Matching Pass

The current evidence is good enough to stop protocol digging and move into the first catalogue matcher.

Approved policy for the first pass:

- require an exact normalized title match
- allow duration drift within `±15 min` (`±900 s`)
- prefer stronger identifiers when present, such as bundle ID, canonical ID, program ID, store ID, or provider content ID
- allow Infuse and Plex to match on title plus duration even though they do not expose a stable app identifier in the current Companion payloads
- treat Netflix as unsupported or low-confidence for now because the current Companion watcher still receives empty payloads there
- keep the result suggestion-only and log it to `.scratch/atv-companion/matches.jsonl`
- never auto-apply BEQ from Companion matching alone, even if only one catalogue row remains
- group the result at the movie level first, then surface the matching BEQ variants under that movie for manual selection

Catalogue/index note:

- the matcher should not grow into a separate long-term catalogue source of truth
- the source of truth remains the external ezBEQ catalogue fetched by `tools/build-catalogue.py`
- if the watcher needs search-friendly fields on desktop or ESP32, extend the derived compact index we already publish for ESP32 rather than keeping a separate watcher-only database
- likely useful added fields for that derived search index are normalized title, original title, duration/runtime, year, content type, and the existing entry digest used to resolve the final filter payload

First replay result from the current captured sample set:

- the matcher now replays real captured Apple TV samples against the upstream ezBEQ `database.json`
- exact normalized title plus duration tolerance `±15 min` is strong enough to find likely movie candidates
- it is not always strong enough to choose one unique final BEQ digest, because the catalogue often contains multiple same-title entries for different audio variants or editions with similar runtimes
- current replay hits:
  - `Example Film C` -> plausible matches found, but ambiguous across multiple digests
  - `Example Film D` -> plausible matches found, but ambiguous across multiple digests
  - `Example Film B` -> plausible matches found, but ambiguous across multiple digests
- current replay misses:
  - `Example Film F`
  - `Example Film A`
  - `Example Film E`
- practical conclusion: title-plus-duration is good enough to drive suggestion mode, but not good enough to auto-pick a final filter variant in every case

Safety note from the current BEQ community guidance:

- AVSForum guidance is to match the BEQ to the actual source audio format or mix
- if the active mix does not match the BEQ entry, the result is considered unsupported and can be risky
- because the current Companion watcher cannot reliably tell us the active app or codec/mix, Companion-only matching should stay in manual suggestion mode

## Phase-0 Persistence

For the disposable spike, do not jump straight to long-term app storage such as `~/.config`.

Keep phase-0 state in a repo-local gitignored scratch directory instead.

Suggested shape:

- `.scratch/atv-companion/devices.json`
- `.scratch/atv-companion/credentials.json`
- `.scratch/atv-companion/metadata.jsonl`
- `.scratch/atv-companion/matches.jsonl`
- `.scratch/atv-companion/debug/`

Why this is the right tradeoff early:

- all spike artifacts stay with the spike
- deleting the experiment is trivial if it fails
- credentials, raw dumps, and match logs are easy to inspect while debugging
- we avoid inventing a production persistence model before the watcher is proven

Once the spike is validated and folded into the main product path, we can revisit final persistence placement and format.

## Validation Gates

Before folding this into the main product path, all of this should be true:

- pairing works reliably on a real Apple TV
- reconnect after process restart works with stored credentials
- the watcher produces stable, useful metadata during real playback
- Infuse and Plex playback have been validated on real hardware
- ezBEQ catalogue matching quality is good enough to rely on

Current decision:

- the watcher is now proven enough to begin the first matcher using exact normalized title plus duration tolerance `±15 min`
- stronger identifiers should override title-plus-duration matching whenever they exist
- automatic apply is out of scope for Companion-only matching, not just deferred
- the preferred automation boundary is now manual apply in, automatic reset out
- if matcher search needs extra catalogue fields, add them to the existing derived ESP32-friendly index rather than building a parallel watcher catalogue
- when the first-pass matcher returns multiple plausible digests for the same title, treat that as an expected ambiguity to surface or resolve later rather than pretending title-plus-duration uniquely identified the final BEQ entry
- even when the first-pass matcher returns a single plausible digest, treat it as a best-guess suggestion rather than a safe auto-apply signal because the actual audio mix is still unknown

Recommended safe automation model:

- never auto-apply a BEQ variant just because a movie match exists
- do allow optional auto-reset back to a user-configured default preset/slot after a watcher-owned movie session ends
- only arm auto-reset when the user manually applies a BEQ variant from the detected-movie flow
- after apply, require the same movie to be observed again before treating the session as active
- once active, keep the session alive while the same movie continues to match
- trigger reset only after that movie is absent for a sustained grace period, not on a single empty or changed payload
- cancel the armed session immediately if the user manually changes preset/slot, clears the BEQ, or applies a different movie variant
- treat Apple TV disconnects, idle state, and unknown metadata as weak exit signals that still need the same grace period before reset

Why this boundary is defensible:

- it gives useful automation in the safe direction only
- it does not pretend the watcher knows the correct BEQ variant on the way in
- it tolerates pause screens, app menus, transient empty metadata, and short reconnect gaps
- it preserves user control whenever the user intentionally changes state

## Product Phases

This section is now the source of truth for the ATV watcher product plan, including the custom UI work.

Product goal:

- pick the one Apple TV to watch
- see what is playing now
- show the detected movie match
- show the matching BEQ variants
- let the user choose the variant to apply
- preserve a default "normal" slot or preset
- support optional auto-reset back to that default after a watcher-owned movie session ends

Product scope decisions already approved:

- support one watched Apple TV per device/product instance
- do not build multi-Apple-TV watcher management into the product plan
- the practical assumption is that a single MiniDSP/receiver chain is tied to one relevant Apple TV
- keep the entry boundary manual:
  - manual apply in
  - automatic reset out by default
  - allow the user to disable automatic reset out
- do not auto-apply BEQ from Companion matching alone

UI direction:

- the next product work should be backend/session lock-in plus an early ESP32 feasibility gate, not more protocol digging
- keep stock ezBEQ available as `/classic`
- do not start a broad frontend rewrite yet
- the custom flow should be Apple-TV-first rather than catalogue-first

Phase 1: desktop watcher lock-in

- keep the current desktop watcher as the source of truth unless something regresses
- define backend session state for the one watched Apple TV, detected movie, candidate variants, chosen variant, default slot or preset, and auto-reset state
- define the safe auto-reset rules:
  - manual apply in
  - auto-reset out enabled by default
  - user can disable auto-reset out
  - watcher-owned session only
  - grace-period reset out
  - manual user changes disarm automation immediately
- confirm the watcher cadence and reconnect behavior are good enough for daily desktop use

Phase 2: early ESP32 feasibility gate

- keep it narrow, disposable, and separate from the main product path
- no custom UI is required for this gate
- first prove the Companion client can run acceptably on the actual ESP32 target:
  - reconnect with persisted credentials
  - perform the Companion handshake
  - fetch and/or subscribe to now-playing metadata
  - keep a long-running watcher alive without obvious memory or stability problems
- if pairing on-device is straightforward, include it in this gate; if not, start with imported credentials and prove reconnect/watch first
- use the existing compact ESP32 catalogue artifacts if any local title/runtime lookup is needed; do not introduce a second watcher-only database
- this is a hard product gate:
  - if the watcher cannot be made reliable enough on ESP32, stop ATV product work before investing in a custom UI
  - if the watcher works on ESP32 well enough, continue to the custom UI phase

Repo-specific implementation guidance for the ESP gate:

- do not try to transplant the desktop spike wholesale into firmware
- the current ESP32 codebase uses blocking `std` plus dedicated worker threads and `Arc<Mutex<...>>` shared state, not a Tokio-style runtime
- the first embedded watcher should follow that existing pattern:
  - one dedicated background watcher service
  - shared watcher state exposed to the rest of the firmware
  - small debug/inspection surface first, before any product UI
- the first embedded milestone should be narrower than the desktop spike:
  - imported credentials are acceptable
  - selected device ID plus manual host is acceptable
  - reconnect plus watch is the real go/no-go test
- keep the embedded scope single-device as well:
  - one watched Apple TV
  - one stored credential record in active use
  - no multi-ATV selection UX is required for this gate
- do not make Bonjour discovery the first embedded hurdle
- do not make on-device pairing the first embedded hurdle
- once reconnect/watch is proven on the board, discovery and pairing can become follow-up embedded slices

Repo-specific persistence guidance for the ESP gate:

- use NVS for small durable watcher config such as:
  - selected Apple TV device ID
  - paired credential blob
  - small reconnect hints or watcher settings
- use LittleFS for file-like watcher data such as:
  - raw metadata samples
  - debug dumps
  - optional exported logs
- keep the same rough split the firmware already uses elsewhere: NVS for small config, LittleFS for larger file-like state

Repo-specific observability guidance for the ESP gate:

- treat runtime observability as part of the feasibility check, not a later polish task
- expose enough watcher state to answer the real kill-question quickly:
  - connected / reconnecting / failed state
  - last successful metadata time
  - reconnect count
  - last error
  - heap free / heap minimum around watcher activity
- if the watcher appears unstable on ESP32, the first response should be more diagnostics, not more product UI work

Phase 3: minimum custom UI slice

- build the smallest product-owned UI needed for the watcher flow while keeping stock ezBEQ available at `/classic`
- first UI scope:
  - Apple TV discovery screen
  - pair / unpair flow
  - watched-device selection for the single active watched Apple TV
  - watcher connection state
  - current now-playing panel
  - matched movie candidate, match confidence, duration delta, and BEQ variants
  - default "normal" slot or preset selection
  - auto-reset enabled toggle (default on)
  - current movie-session state: armed, pending confirmation, active, grace, disarmed
  - actions such as `Apply`, `Apply And Auto-Reset`, `Reset Slot`, and `Remember This Variant`

Phase 4: safe automation

- keep entry manual: no automatic BEQ apply from Companion matching alone
- automatic reset out is the default behavior after a watcher-owned movie session ends
- allow the user to disable automatic reset out
- tune the grace-period behavior against real pause/menu/reconnect cases

Phase 5: desktop soak testing

- use the full flow on real Infuse and Plex playback over time
- tune the exit rules and grace windows
- confirm the default-slot behavior feels right in daily use
- confirm the narrow custom UI already solves the main workflow cleanly

Phase 6: broader product integration

- only after the desktop and ESP32 watcher flows are stable, decide what pieces to fold into the main product path
- only after that, decide whether a broader main-UI replacement is warranted
- only after that, do the real product integration work rather than keeping ATV in spike-shaped slices

ESP timing clarification:

- ATV phase 0 is the desktop-native proof that already established pair, reconnect, and now-playing capture
- the next product steps are: backend session model first, then an early ESP32 feasibility gate, then the narrow custom UI only if ESP passes
- the ESP32 check is no longer a late follow-up; it is an early go/no-go decision for the entire ATV feature
- budget the ESP32 feasibility gate against the current dual-slot layout: two 3 MB app slots and about 9.9 MB data, with the current ezBEQ bundle about 6.9 MB; remeasure watcher headroom rather than using the old single-app layout
- if watcher matching needs extra search fields on ESP32 later, extend the existing compact catalogue artifacts rather than introducing a second watcher-only database

Before attempting the custom ATV UI, all of this should be true:

- the desktop watcher is already proven end to end
- we have at least one acceptable ESP32 watcher path that proves reconnect/watch is viable on-device
- we know which code is actually worth sharing
- we know the real persistence and reconnect requirements
- we know the matching and apply policy that the embedded version would need

Kill criteria for the spike should stay explicit:

- pairing is too brittle to rely on
- metadata is too weak or inconsistent to match usefully
- the watcher cannot be made acceptably stable or memory-safe on ESP32
- the required implementation path is not something we would ship

## Bottom Line

The facts support a very specific conclusion:

- Apple does have a real now-playing observer/client path for Apple TV.
- Apple does not appear to publish that path as a public third-party API.
- Apple’s own private frameworks, plus independent Companion implementations, show that the modern solution is a paired encrypted client session.
- Passive same-LAN sniffing is not enough for useful playback observation.
- Reverse engineering and product implementation are still very feasible, but the right model is a read-only paired observer or an active proxy for protocol study.

For this project, the best next step is not "find a passive snoop trick." It is:

- treat the current desktop Companion watcher as good enough to move product work forward
- Phase 1: lock the desktop watcher and backend session model
- Phase 2: run the early ESP32 feasibility gate and treat it as a go/no-go check for the feature
- Phase 3: only if ESP passes, implement the minimum custom UI slice in this ATV plan while keeping stock ezBEQ available as `/classic`
- Phase 4: add the safe automation boundary of manual apply in, automatic reset out
- Phase 5: do real soak testing, tune grace periods, and verify the daily-use flow
- only then fold stable pieces into the main product path

## Source Index

### Official Apple Sources

- Apple Support: <https://support.apple.com/en-ca/108778>
- Apple Support: <https://support.apple.com/en-ca/guide/tv/atvb701cadc1/26/tvos/26>
- Apple Support: <https://support.apple.com/en-ca/guide/tv/atvb7944597f/26/tvos/26>
- Apple Developer Bonjour: <https://developer.apple.com/bonjour/>
- Apple Developer Bonjour architecture archive: <https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/NetServices/Articles/programming.html>
- Apple Developer AVFoundation: <https://developer.apple.com/documentation/avfoundation/supporting-remote-interactions-in-tvos>
- WWDC 2017: <https://developer.apple.com/videos/play/wwdc2017/251/>
- WWDC 2022: <https://developer.apple.com/videos/play/wwdc2022/110338/>
- Apple Platform Security: <https://support.apple.com/en-ca/guide/security/sec3a881ccb1/web>
- Apple Platform Security: <https://support.apple.com/en-ca/guide/security/sec5d283419e/web>
- Apple Developer packet traces: <https://developer.apple.com/documentation/network/recording-a-packet-trace>
- Apple Developer packet traces: <https://developer.apple.com/documentation/network/troubleshooting-packet-traces>

### Apple-Origin Private Artifacts

- `TVRemoteCore.tbd` from Xcode macOS 26.2 SDK
- `MediaRemote.tbd` from Xcode macOS 26.2 SDK
- `Rapport.tbd` from Xcode macOS 26.2 SDK
- `TVRCRPCompanionLinkClientWrapper.h`
  - <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/PrivateFrameworks/TVRemoteCore.framework/TVRCRPCompanionLinkClientWrapper.h>
- `TVRCDevice.h`
  - <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/PrivateFrameworks/TVRemoteCore.framework/TVRCDevice.h>
- `TVRCDeviceDelegate.h`
  - <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/protocols/TVRCDeviceDelegate.h>
- `TVRCNowPlayingMetadata.h`
  - <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/PrivateFrameworks/TVRemoteCore.framework/TVRCNowPlayingMetadata.h>
- `TVRCRapportMediaEventsManager.h`
  - <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/PrivateFrameworks/TVRemoteCore.framework/TVRCRapportMediaEventsManager.h>
- `RPCompanionLinkDevice.h`
  - <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/PrivateFrameworks/Rapport.framework/RPCompanionLinkDevice.h>
- `MRRapportTransport.h`
  - <https://raw.githubusercontent.com/qingralf/iOS18-Runtime-Headers/master/PrivateFrameworks/MediaRemote.framework/MRRapportTransport.h>

### Reverse-Engineered and Open-Source Sources

- `pyatv` protocols: <https://pyatv.dev/documentation/protocols/#companion-link>
- `pyatv` scan/pair/connect: <https://pyatv.dev/development/scan_pair_and_connect/>
- `pyatv` supported features: <https://pyatv.dev/documentation/supported_features/>
- `pyatv` FAQ: <https://pyatv.dev/support/faq/>
- `pyatv` proxy: <https://pyatv.dev/documentation/atvproxy/>
- `pyatv` repository: <https://github.com/postlund/pyatv>
- `pyatv` issue on legacy Home Sharing discovery: <https://github.com/postlund/pyatv/issues/214>
- `basmilius/apple-protocols`: <https://github.com/basmilius/apple-protocols>
- `LaurentVoillot/AppleTVr`: <https://github.com/LaurentVoillot/AppleTVr>
- `jeanregisser/mediaremotetv-protocol`: <https://github.com/jeanregisser/mediaremotetv-protocol>
- Unofficial AirPlay service discovery spec: <https://openairplay.github.io/airplay-spec/service_discovery.html>

### Adjacent Product Sources

- Infuse third-party API: <https://support.firecore.com/hc/en-us/articles/215090997-API-for-Third-Party-Apps-Services>
- Infuse media-server sync: <https://support.firecore.com/hc/en-us/articles/4405110679447-Media-Server-Sync>
- Plex webhooks: <https://support.plex.tv/articles/115002267687-webhooks/>
- Trakt API: <https://trakt.docs.apiary.io/>
