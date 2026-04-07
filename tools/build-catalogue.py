#!/usr/bin/env python3
"""Build compact ezBEQ catalogue artifacts for ESP32.

Downloads the beqcatalogue database.json and generates:
  catalogue.bin    — Binary biquad index keyed by digest hash prefix.
                     Sorted for O(1) binary search on ESP32.
                     Header + per-entry: digest_prefix(8) + filter_count(u8) +
                     mv(f32) + coeffs(10 × 5 × f32).
  manifest.json    — Version metadata for ESP32 auto-update.
  version.txt      — Catalogue version for update checks.
  ui/              — ezBEQ React UI from PyPI with JS shim injected.
  ui/ezbeq-shim.js — Browser-side catalogue loader (fetches database.json
                     directly from beqcatalogue, bypassing ESP32).

The browser loads the full catalogue directly from beqcatalogue.readthedocs.io.
The ESP32 only stores catalogue.bin (biquad lookup) and the UI files.

Usage:
    uv run tools/build-catalogue.py [--output-dir DIR]
"""

import hashlib
import json
import math
import struct
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path

CATALOGUE_URL = "https://raw.githubusercontent.com/3ll3d00d/beqcatalogue/master/docs/database.json"
VERSION_URL = "https://raw.githubusercontent.com/3ll3d00d/beqcatalogue/master/docs/version.txt"
PYPI_JSON_URL = "https://pypi.org/pypi/ezbeq/json"

# Binary format constants (v2: digest-keyed)
BIN_MAGIC = b"BEQ\x02"
SAMPLE_RATE = 96000
MAX_FILTERS = 10
COEFFS_PER_FILTER = 5  # b0, b1, b2, a1, a2
DIGEST_PREFIX_SIZE = 8  # First 8 bytes of SHA256
# Per-entry record: 8 bytes digest_prefix + 1 byte filter_count + 4 bytes mv_adjust + 10*5*4 bytes coefficients
RECORD_SIZE = DIGEST_PREFIX_SIZE + 1 + 4 + MAX_FILTERS * COEFFS_PER_FILTER * 4  # 213 bytes

# beqcatalogue base URL for browser-side fetching
CATALOGUE_BASE_URL = "https://beqcatalogue.readthedocs.io/en/latest"


def compute_biquad_coefficients(filt: dict, fs: int) -> list[float]:
    """Compute biquad coefficients from parametric filter description.

    Returns [b0, b1, b2, a1, a2] with a1/a2 negated (minidsp convention).
    """
    freq = filt["freq"]
    gain = filt["gain"]
    q = filt["q"]
    ftype = filt["type"]

    w0 = 2.0 * math.pi * freq / fs
    cos_w0 = math.cos(w0)
    sin_w0 = math.sin(w0)
    alpha = sin_w0 / (2.0 * q)
    A = 10.0 ** (gain / 40.0)  # sqrt(10^(gain/20))

    if ftype == "PeakingEQ":
        b0 = 1.0 + alpha * A
        b1 = -2.0 * cos_w0
        b2 = 1.0 - alpha * A
        a0 = 1.0 + alpha / A
        a1 = -2.0 * cos_w0
        a2 = 1.0 - alpha / A
    elif ftype == "LowShelf":
        sq = 2.0 * math.sqrt(A) * alpha
        b0 = A * ((A + 1.0) - (A - 1.0) * cos_w0 + sq)
        b1 = 2.0 * A * ((A - 1.0) - (A + 1.0) * cos_w0)
        b2 = A * ((A + 1.0) - (A - 1.0) * cos_w0 - sq)
        a0 = (A + 1.0) + (A - 1.0) * cos_w0 + sq
        a1 = -2.0 * ((A - 1.0) + (A + 1.0) * cos_w0)
        a2 = (A + 1.0) + (A - 1.0) * cos_w0 - sq
    elif ftype == "HighShelf":
        sq = 2.0 * math.sqrt(A) * alpha
        b0 = A * ((A + 1.0) + (A - 1.0) * cos_w0 + sq)
        b1 = -2.0 * A * ((A - 1.0) + (A + 1.0) * cos_w0)
        b2 = A * ((A + 1.0) + (A - 1.0) * cos_w0 - sq)
        a0 = (A + 1.0) - (A - 1.0) * cos_w0 + sq
        a1 = 2.0 * ((A - 1.0) - (A + 1.0) * cos_w0)
        a2 = (A + 1.0) - (A - 1.0) * cos_w0 - sq
    else:
        raise ValueError(f"Unknown filter type: {ftype}")

    # Normalize by a0, negate a1/a2 (minidsp convention)
    return [b0 / a0, b1 / a0, b2 / a0, -(a1 / a0), -(a2 / a0)]


def get_biquad_coefficients(filt: dict, fs: int) -> list[float]:
    """Get biquad coefficients, using pre-computed if available for this sample rate."""
    fs_key = str(fs)
    if "biquads" in filt and fs_key in filt["biquads"]:
        bq = filt["biquads"][fs_key]
        b = [float(x) for x in bq["b"]]
        a = [float(x) for x in bq["a"]]
        # a coefficients in catalogue are already negated
        return b + a
    # Recompute from parametric description
    return compute_biquad_coefficients(filt, fs)


def entry_digest(entry: dict) -> str:
    """Get or compute the digest for a catalogue entry."""
    if "digest" in entry and entry["digest"]:
        return entry["digest"]
    # Fallback: compute from key fields (matches beqcatalogue's approach)
    key = json.dumps(
        {k: entry.get(k) for k in ("title", "filters", "mv", "season", "episode")},
        sort_keys=True,
        separators=(",", ":"),
    )
    return hashlib.sha256(key.encode()).hexdigest()


def build_binary_record(entry: dict, fs: int) -> bytes:
    """Build a fixed-size binary record for one entry's biquad data.

    Format: digest_prefix(8) + filter_count(u8) + mv_adjust(f32) + 10×5×f32
    """
    digest_hex = entry_digest(entry)
    digest_prefix = bytes.fromhex(digest_hex[:DIGEST_PREFIX_SIZE * 2])  # 16 hex chars = 8 bytes

    filters = entry.get("filters", [])
    mv = float(entry.get("mv", "0"))
    count = min(len(filters), MAX_FILTERS)

    # Pack: digest_prefix + filter_count(u8) + mv_adjust(f32) + coefficients(10 × 5 × f32)
    data = digest_prefix + struct.pack("<Bf", count, mv)
    for i in range(MAX_FILTERS):
        if i < count:
            coeffs = get_biquad_coefficients(filters[i], fs)
            data += struct.pack("<5f", *coeffs)
        else:
            data += b"\x00" * (COEFFS_PER_FILTER * 4)
    assert len(data) == RECORD_SIZE, f"Record size mismatch: {len(data)} != {RECORD_SIZE}"
    return data


def download_ui(output_dir: Path) -> tuple[str, dict[str, dict]]:
    """Download the ezBEQ React UI from the PyPI wheel.

    Returns (ui_version, {relative_path: {size, sha256}}).
    """
    # Find latest wheel URL from PyPI
    print("Fetching ezBEQ UI from PyPI...")
    with urllib.request.urlopen(PYPI_JSON_URL, timeout=15) as resp:
        pypi = json.loads(resp.read())
    ui_version = pypi["info"]["version"]

    # Find the .whl URL (wheels are zip files)
    whl_url = None
    for url_info in pypi["urls"]:
        if url_info["filename"].endswith(".whl"):
            whl_url = url_info["url"]
            break
    if not whl_url:
        raise RuntimeError("No wheel found on PyPI for ezbeq")

    print(f"  version: {ui_version}")
    print(f"  wheel: {whl_url.split('/')[-1]}")

    # Download wheel to temp file and extract ui/ files
    ui_dir = output_dir / "ui"
    ui_dir.mkdir(exist_ok=True)

    with tempfile.NamedTemporaryFile(suffix=".whl") as tmp:
        with urllib.request.urlopen(whl_url, timeout=60) as resp:
            tmp.write(resp.read())
        tmp.flush()

        with zipfile.ZipFile(tmp.name) as zf:
            ui_files = [n for n in zf.namelist() if n.startswith("ezbeq/ui/") and not n.endswith("/")]
            for name in ui_files:
                # Strip "ezbeq/ui/" prefix -> relative path under ui/
                rel = name[len("ezbeq/ui/"):]
                # Zip slip protection: reject paths with traversal or absolute components
                if ".." in rel.split("/") or rel.startswith("/"):
                    print(f"  WARNING: skipping suspicious zip entry: {name}")
                    continue
                dest = ui_dir / rel
                if not str(dest.resolve()).startswith(str(ui_dir.resolve())):
                    print(f"  WARNING: skipping zip entry escaping ui/: {name}")
                    continue
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(zf.read(name))

    # Inject the JS shim into index.html
    inject_shim(ui_dir)

    # Build file manifest entries (paths relative to output_dir, prefixed with ui/)
    files = {}
    ui_total = 0
    for path in sorted(ui_dir.rglob("*")):
        if path.is_file():
            rel = str(path.relative_to(output_dir))
            size = path.stat().st_size
            files[rel] = {
                "size": size,
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
            ui_total += size

    print(f"  {len(files)} files, {ui_total:,} bytes ({ui_total/1024:.0f}KB)")
    return ui_version, files


def inject_shim(ui_dir: Path):
    """Inject the ezbeq-shim.js script tag into index.html and write the shim file."""
    index_path = ui_dir / "index.html"
    if not index_path.exists():
        print("  WARNING: no index.html found, skipping shim injection")
        return

    # Write the shim JS file
    shim_path = ui_dir / "ezbeq-shim.js"
    shim_path.write_text(EZBEQ_SHIM_JS, encoding="utf-8")
    print(f"  wrote ezbeq-shim.js ({len(EZBEQ_SHIM_JS):,} bytes)")

    # Inject script tag before </head>
    html = index_path.read_text(encoding="utf-8")
    if "ezbeq-shim.js" in html:
        return  # Already injected
    new_html = html.replace("</head>", '<script src="/ezbeq-shim.js"></script>\n</head>')
    if new_html == html:
        print("  WARNING: </head> not found in index.html, shim NOT injected")
        return
    index_path.write_text(new_html, encoding="utf-8")
    print("  injected shim into index.html")


# ---------------------------------------------------------------------------
# JS Shim: browser-side catalogue loader
# ---------------------------------------------------------------------------

EZBEQ_SHIM_JS = r"""/**
 * ezbeq-shim.js — Browser-direct catalogue loading for minidsp-open ESP32.
 *
 * Intercepts the ezBEQ React UI's WebSocket "load catalogue" request and
 * fetches database.json directly from beqcatalogue.readthedocs.io instead
 * of asking the ESP32 to stream it. Also enriches filter PUT requests with
 * title/author metadata from the cached catalogue.
 *
 * Generated by tools/build-catalogue.py — do not edit manually.
 */
(function() {
  'use strict';

  const CATALOGUE_BASE = '""" + CATALOGUE_BASE_URL + r"""';
  const DB_NAME = 'ezbeq-cache';
  const STORE_NAME = 'catalogue';
  const CHUNK_SIZE = 100;

  // In-memory entry map: { digest -> entry } for PUT enrichment
  let entryMap = {};

  // --- IndexedDB helpers ---

  function openDB() {
    return new Promise((resolve, reject) => {
      const req = indexedDB.open(DB_NAME, 1);
      req.onupgradeneeded = () => req.result.createObjectStore(STORE_NAME);
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
  }

  function idbGet(db, key) {
    return new Promise((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, 'readonly');
      const req = tx.objectStore(STORE_NAME).get(key);
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
  }

  function idbPut(db, key, value) {
    return new Promise((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, 'readwrite');
      const req = tx.objectStore(STORE_NAME).put(value, key);
      req.onsuccess = () => resolve();
      req.onerror = () => reject(req.error);
    });
  }

  // --- Catalogue processing ---

  function processEntry(entry) {
    // Strip bulky fields not needed for display
    delete entry.filters;

    // Set id to digest (React UI keys entries by .id)
    // Fallback for entries without digest: composite key (won't match catalogue.bin, but avoids empty IDs)
    entry.id = entry.digest || (entry.title + '_' + entry.year + '_' + (entry.audioTypes || []).join('_'));

    // Compatibility fields expected by ezBEQ React UI
    entry.contentType = entry.content_type || 'film';
    entry.mvAdjust = parseFloat(entry.mv || '0');

    // Formatted title with season/episode for TV content
    let fmt = entry.title || '';
    if (entry.season) {
      fmt += ' S' + entry.season;
      if (entry.episode) fmt += 'E' + entry.episode;
    }
    entry.formattedTitle = fmt;

    return entry;
  }

  async function loadCatalogue(ws) {
    let entries = null;

    try {
      const db = await openDB();

      // Check cached version
      const cachedVersion = await idbGet(db, 'version').catch(() => null);
      let remoteVersion = null;
      try {
        const vResp = await fetch(CATALOGUE_BASE + '/version.txt');
        if (vResp.ok) remoteVersion = (await vResp.text()).trim();
      } catch (e) { /* offline — use cache if available */ }

      if (cachedVersion && (remoteVersion === null || remoteVersion === cachedVersion)) {
        // Use cached entries
        entries = await idbGet(db, 'entries').catch(() => null);
        if (entries) {
          console.log('[ezbeq-shim] using cached catalogue v' + cachedVersion +
                      ' (' + entries.length + ' entries)');
        }
      }

      if (!entries) {
        // Fetch fresh catalogue
        console.log('[ezbeq-shim] fetching database.json from ' + CATALOGUE_BASE + '...');
        const resp = await fetch(CATALOGUE_BASE + '/database.json');
        if (!resp.ok) throw new Error('HTTP ' + resp.status);
        const raw = await resp.json();
        entries = raw.map(processEntry);

        // Cache in IndexedDB
        const version = remoteVersion || 'unknown';
        await idbPut(db, 'version', version);
        await idbPut(db, 'entries', entries);
        console.log('[ezbeq-shim] cached ' + entries.length + ' entries (v' + version + ')');
      }
    } catch (err) {
      console.error('[ezbeq-shim] catalogue load failed:', err);
      // Dispatch error to React UI
      const errMsg = JSON.stringify({
        message: 'Error',
        data: 'Could not load catalogue. Check internet connection.'
      });
      ws.dispatchEvent(new MessageEvent('message', { data: errMsg }));
      return;
    }

    // Build in-memory entry map for PUT enrichment
    entryMap = {};
    for (const e of entries) {
      if (e.id) entryMap[e.id] = e;
    }

    // Dispatch entries to React UI in chunks
    for (let i = 0; i < entries.length; i += CHUNK_SIZE) {
      const chunk = entries.slice(i, i + CHUNK_SIZE);
      const msg = JSON.stringify({ message: 'CatalogueEntries', data: chunk });
      ws.dispatchEvent(new MessageEvent('message', { data: msg }));
      // Yield to UI thread between chunks
      await new Promise(r => setTimeout(r, 0));
    }
    console.log('[ezbeq-shim] dispatched ' + entries.length + ' entries to UI');
  }

  // --- WebSocket monkey-patch ---

  const OrigWebSocket = window.WebSocket;

  window.WebSocket = function(url, protocols) {
    const ws = protocols !== undefined
      ? new OrigWebSocket(url, protocols)
      : new OrigWebSocket(url);

    // Only shim the ezBEQ /ws endpoint (not /devices/0/ws raw bridge)
    if (!url.endsWith('/ws')) return ws;

    const origSend = ws.send.bind(ws);
    ws.send = function(data) {
      if (typeof data === 'string' && data.trim() === 'load catalogue') {
        // Intercept: load catalogue from beqcatalogue directly
        loadCatalogue(ws);
        return;
      }
      origSend(data);
    };

    return ws;
  };

  // Preserve prototype chain for instanceof checks
  window.WebSocket.prototype = OrigWebSocket.prototype;
  window.WebSocket.CONNECTING = OrigWebSocket.CONNECTING;
  window.WebSocket.OPEN = OrigWebSocket.OPEN;
  window.WebSocket.CLOSING = OrigWebSocket.CLOSING;
  window.WebSocket.CLOSED = OrigWebSocket.CLOSED;

  // --- fetch() interceptor for PUT enrichment ---

  const origFetch = window.fetch;

  window.fetch = function(url, opts) {
    if (typeof url === 'string' &&
        /\/api\/\d+\/devices\/[^/]+\/filter\/\d+/.test(url) &&
        opts && opts.method && opts.method.toUpperCase() === 'PUT') {
      try {
        const body = JSON.parse(opts.body);
        if (body.entryId && entryMap[body.entryId]) {
          const entry = entryMap[body.entryId];
          body.title = entry.formattedTitle || entry.title || 'Unknown';
          body.author = entry.author || 'Unknown';
          opts = Object.assign({}, opts, { body: JSON.stringify(body) });
        }
      } catch (e) { /* pass through unmodified */ }
    }
    return origFetch.call(this, url, opts);
  };

  console.log('[ezbeq-shim] loaded — catalogue will be fetched from ' + CATALOGUE_BASE);
})();
"""


def main():
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 and sys.argv[1] == "--output-dir" else Path(".")

    # Download catalogue
    print(f"Downloading catalogue from {CATALOGUE_URL}...")
    with urllib.request.urlopen(CATALOGUE_URL, timeout=60) as resp:
        raw = resp.read()
    db = json.loads(raw)
    print(f"  {len(db)} entries")

    # Download version
    try:
        with urllib.request.urlopen(VERSION_URL, timeout=10) as resp:
            version = resp.read().decode().strip()
    except Exception:
        version = hashlib.sha256(raw).hexdigest()[:12]
    print(f"  version: {version}")

    # Build binary biquad index (sorted by digest prefix for binary search)
    print("Building binary index...")
    records = []
    skipped = 0
    for entry in db:
        digest = entry_digest(entry)
        try:
            record = build_binary_record(entry, SAMPLE_RATE)
            records.append((bytes.fromhex(digest[:DIGEST_PREFIX_SIZE * 2]), record))
        except Exception as e:
            skipped += 1
            if skipped <= 3:
                print(f"  WARNING: skipping entry '{entry.get('title', '?')}': {e}")

    # Sort by digest prefix for binary search
    records.sort(key=lambda r: r[0])

    bin_path = output_dir / "catalogue.bin"
    with open(bin_path, "wb") as f:
        # Header: magic(4) + entry_count(u32) + sample_rate(u32)
        f.write(BIN_MAGIC)
        f.write(struct.pack("<II", len(records), SAMPLE_RATE))
        # Per-entry records (sorted by digest prefix)
        for _, record in records:
            f.write(record)
    bin_size = bin_path.stat().st_size
    print(f"  catalogue.bin: {bin_size:,} bytes ({bin_size/1024/1024:.1f}MB), {len(records)} entries")
    if skipped:
        print(f"  ({skipped} entries skipped due to errors)")

    # Write version.txt (for GH Action version check and ESP32)
    version_path = output_dir / "version.txt"
    version_path.write_text(version + "\n")

    # Download ezBEQ React UI from PyPI
    ui_version, ui_files = download_ui(output_dir)

    # Build manifest — catalogue.bin + UI files
    files = {
        "catalogue.bin": {
            "size": bin_size,
            "sha256": hashlib.sha256(bin_path.read_bytes()).hexdigest(),
        },
        "version.txt": {
            "size": version_path.stat().st_size,
            "sha256": hashlib.sha256(version_path.read_bytes()).hexdigest(),
        },
    }
    files.update(ui_files)

    manifest = {
        "catalogue_version": version,
        "ui_version": ui_version,
        "entry_count": len(records),
        "files": files,
    }
    manifest_path = output_dir / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"  manifest.json: {len(files)} files")

    total = sum(f["size"] for f in files.values())
    print(f"\nTotal: {total:,} bytes ({total/1024/1024:.1f}MB)")


if __name__ == "__main__":
    main()
