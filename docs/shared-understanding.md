# How The Three Projects Work Together

Three separate repos, one calibration workflow. The end goal is a single app where a user plugs in a mic, clicks through a wizard, and gets perfectly optimized subs + Dirac room correction on their full system — no REW, no MSO, no MiniDSP Plugin, no fighting Dirac's UI.

## The Projects

### 1. minidsp-open (this repo)
**What:** Clean-room MiniDSP hardware controller. Rust. HTTP API + CLI.
**Role:** The device control layer. Talks USB HID / TCP to MiniDSP hardware. Reads and writes every DSP parameter — PEQ biquads, gain, delay, polarity, crossover, volume, source, presets, config read-back.
**Replaces:** minidsp-rs (an upstream protocol reference) + MiniDSP Plugin (closed-source GUI) + $75 WI-DG WiFi bridge (ESP32 target).
**Repo:** `~/code/minidsp-open`

### 2. open-sub-optimizer
**What:** Multi-subwoofer optimizer. Electron + React + TypeScript.
**Role:** Stage 1 of calibration. Measures each sub individually at multiple seats, runs a JADE/EPSDE optimizer to compute optimal delay, gain, polarity, and PEQ per sub, then pushes the config to MiniDSP hardware. Replaces the REW + MSO + MiniDSP Plugin three-app shuffle.
**Repo:** `~/code/open-sub-optimizer`

### 3. dirac-sparr
**What:** Automated Dirac Live calibration client. Electron + React + TypeScript.
**Role:** Stage 2 of calibration + host app. Auto-discovers the receiver, pre-configures it via eISCP, guides mic placement, runs measurement sweeps, sends to Dirac's cloud for filter computation, uploads FIR filters to the receiver. Imports open-sub-optimizer as an npm package for optional Stage 1 sub optimization. Discovers minidsp-open on the network via UDP broadcast (port 3999), mDNS (`_minidsp._tcp`), or manual IP entry — does NOT bundle or manage minidsp-open.
**Repo:** `~/code/dirac-sparr`

## The Calibration Flow

```
                    minidsp-open (API)
                         │
          ┌──────────────┼──────────────┐
          │              │              │
    ┌─────▼─────┐  ┌─────▼─────┐  ┌────▼────┐
    │  Measure   │  │ Optimize  │  │  Apply  │
    │  each sub  │  │ JADE/DE   │  │ biquads │
    │  per seat  │  │ algorithm │  │ to DSP  │
    └─────┬─────┘  └─────┬─────┘  └────┬────┘
          │              │              │
          └──────────────┼──────────────┘
                         │
               open-sub-optimizer
              (Stage 1: Sub Optimization)
                         │
                         ▼
                   Subs are now
              delay/gain/PEQ-optimized
                         │
                         ▼
               ┌─────────────────────┐
               │     dirac-sparr       │
               │  (Stage 2: Dirac)   │
               ├─────────────────────┤
               │ 1. Discover receiver│
               │ 2. Set levels/xover │
               │ 3. Volume cal       │
               │ 4. Measure sweeps   │
               │ 5. Cloud → FIR      │
               │ 6. Upload filters   │
               │ 7. Verify           │
               └─────────────────────┘
```

## Why This Order Matters

**Sub optimization (Stage 1) must complete before Dirac (Stage 2).**

Dirac measures the full system acoustically — mains + subs + room. If the subs aren't already optimized when Dirac measures, they may be cancelling each other at certain frequencies, and seat-to-seat variation is high. Dirac can only correct for an average — it can't fix per-sub phase problems.

After sub optimization, Dirac sees one well-behaved "virtual subwoofer" — coherent summation, consistent across seats. Dirac's job becomes much easier and the result is better.

## Dependencies

```
minidsp-open ◄──── open-sub-optimizer
    │                    │
    │                    │ (Stage 1 → Stage 2)
    │                    ▼
    └────────────── dirac-sparr
```

- **open-sub-optimizer** depends on **minidsp-open** for all hardware control
- **dirac-sparr** talks directly to the receiver (DiracIO gRPC) — independent of minidsp-open for Dirac calibration. Discovers minidsp-open on the network for optional Stage 1 sub optimization. Does not bundle or spawn minidsp-open.
- **minidsp-open** is useful standalone (CLI, API, ESP32 WiFi bridge) independent of the other two. Both open-sub-optimizer and dirac-sparr discover and consume it as an external HTTP service.

## The User's Setup

```
Receiver (Onkyo RZ50)
├── Speaker outputs → Mains (7 channels)
└── Sub pre-out ──→ MiniDSP 2x4 HD ──→ Sub 1, Sub 2, Sub 3
                         (USB to PC)
```

Receiver handles mains + Dirac (FIR filters in receiver DSP). MiniDSP handles sub optimization only (PEQ biquads, 10 bands per output). UMIK-1 mic used for both stages.

## Build Order

1. **dirac-sparr** — the main app, useful standalone for anyone with a Dirac receiver
2. **minidsp-open** — hardware control layer, needed before open-sub-optimizer can exist
3. **open-sub-optimizer** — depends on minidsp-open for all hardware control
4. **dirac-sparr v2** — import open-sub-optimizer npm package, add MiniDSP network discovery (UDP/mDNS), build Stage 1 UI in the wizard

## End State

dirac-sparr is the main app. minidsp-open is an independent service the user already has running (desktop daemon or ESP32 box on the network). dirac-sparr discovers it via UDP broadcast (port 3999), mDNS, or manual IP, then talks to it via HTTP for Stage 1 sub optimization. If no minidsp daemon is found, Stage 1 is unavailable and dirac-sparr skips straight to Dirac room correction.

The user with MiniDSP sees:
1. Plug in mic. minidsp-open is already running on their machine or ESP32.
2. dirac-sparr discovers receiver + minidsp daemon on the network.
3. "Optimizing your subs" — measure each sub at each seat, optimize, push config via minidsp-open HTTP API
4. "Dirac room correction" — guided mic placement + sweeps + cloud filters
5. Verification — before/after comparison
6. Done
