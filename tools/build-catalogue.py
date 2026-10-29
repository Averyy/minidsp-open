#!/usr/bin/env python3
"""Build compact ezBEQ catalogue artifacts for ESP32.

Downloads the beqcatalogue database.json and generates:
  catalogue.bin    — Binary biquad index keyed by digest hash prefix.
                     Sorted for O(log n) binary search on ESP32.
                     Header + digest/offset index + variable-length records.
  database.json    — Full catalogue JSON published for upstream clients.
  database.json.gz — Slim offline browser catalogue stored on the ESP32.
  overviews.json.gz — Pages-only summaries, never installed on the ESP32.
  entry-metadata.jsonl — [id, title, author] rows that catalogue.bin points at.
  recent-changes.jsonl — Last 180 days of changes, sorted, for the What's New API.
  manifest.json    — Version metadata for ESP32 auto-update.
  version.txt      — Catalogue version for update checks.
  ui/              — ezBEQ React UI from PyPI with JS shim injected.
  ui/ezbeq-shim.js — Browser-side catalogue loader that fetches the
                     same-origin catalogue with persistent browser caching.

The browser loads the slim catalogue through the board's same-origin
`/database.json` endpoint. Summaries are fetched separately when online.

Usage:
    uv run tools/build-catalogue.py [--output-dir DIR]
"""

import hashlib
import gzip
import json
import math
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import urllib.request
import zipfile
from datetime import datetime, timezone
from pathlib import Path

CATALOGUE_URL = "https://raw.githubusercontent.com/3ll3d00d/beqcatalogue/master/docs/database.json"
VERSION_URL = "https://raw.githubusercontent.com/3ll3d00d/beqcatalogue/master/docs/version.txt"
CATALOGUE_COMMIT_API_URL = "https://api.github.com/repos/3ll3d00d/beqcatalogue/commits/{sha}"
UI_VERSION = "2.12.4"
UI_WHEEL_SHA256 = "72873c702dbdcf0b55d00b63595d3580fc750cafc8e27c9f7d89efc5e5c66873"
PYPI_JSON_URL = f"https://pypi.org/pypi/ezbeq/{UI_VERSION}/json"
DEVICE_BASE_URL = "https://raw.githubusercontent.com/3ll3d00d/beqcatalogue/master/docs/devices/"
CATALOGUE_LICENSE_URL = "https://raw.githubusercontent.com/3ll3d00d/beqcatalogue/master/LICENSE"
SHIM_PATH = Path(__file__).resolve().parent.parent / "app-esp32/static/ezbeq-shim.js"
BUNDLE_BUDGET = 8_000_000
TWO_WEEKS = 14 * 24 * 60 * 60

# Binary v3: header + sorted (digest prefix, absolute record offset) index.
# Records contain flags/count, MV and only the authored number of biquads.
BIN_MAGIC = b"BEQ\x03"
RECENT_CHANGES_WINDOW_SEC = 180 * 24 * 60 * 60
SAMPLE_RATE = 96000
MAX_FILTERS = 10
COEFFS_PER_FILTER = 5  # b0, b1, b2, a1, a2
DIGEST_PREFIX_SIZE = 8  # First 8 bytes of SHA256


def fetch_bytes(url: str, timeout: int = 15) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "minidsp-open build-catalogue"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read()


def fetch_json(url: str, timeout: int = 15) -> dict:
    return json.loads(fetch_bytes(url, timeout=timeout))


def iso_to_build_stamp(value: str) -> str:
    dt = datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(timezone.utc)
    return dt.strftime("%Y%m%dT%H%M%SZ")


def current_build_stamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def repo_build_stamp() -> str:
    repo_root = Path(__file__).resolve().parent.parent
    try:
        dirty = subprocess.run(
            ["git", "status", "--porcelain"],
            cwd=repo_root,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        if dirty:
            return current_build_stamp()
        head_iso = subprocess.run(
            ["git", "log", "-1", "--format=%cI", "HEAD"],
            cwd=repo_root,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        if head_iso:
            return iso_to_build_stamp(head_iso)
    except Exception:
        pass
    return current_build_stamp()


def catalogue_build_stamp(version: str) -> str:
    commit = fetch_json(CATALOGUE_COMMIT_API_URL.format(sha=version), timeout=15)
    return iso_to_build_stamp(commit["commit"]["committer"]["date"])

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


def deduplicate_entries(entries: list[dict]) -> list[dict]:
    # Upstream's catalogue import retains the first entry for each full digest.
    # Keep listing/count/feed/coefficient identity consistent with that policy.
    unique = {}
    for entry in entries:
        unique.setdefault(entry_digest(entry), entry)
    return list(unique.values())


def build_browser_catalogue(entries: list[dict], *, slim: bool = True) -> bytes:
    """Build the browser metadata payload used by the hosted ezBEQ UI.

    This mirrors the shim's processEntry logic but strips filter coefficients so
    the browser catalogue can live on the ESP32 data partition in compressed
    form while the board still resolves real biquads from catalogue.bin.
    """
    processed = []
    now = datetime.now(timezone.utc).timestamp()
    for entry in entries:
        item = dict(entry)
        item.pop("filters", None)
        item["id"] = entry_digest(entry)
        item["year"] = int(item.get("year") or 0)
        item["contentType"] = item.get("content_type") or "film"
        try:
            item["mvAdjust"] = float(item.get("mv") or "0")
        except Exception:
            item["mvAdjust"] = 0.0

        collection = item.get("collection")
        if isinstance(collection, str):
            item["collection"] = collection
        elif isinstance(collection, dict) and isinstance(collection.get("name"), str):
            item["collection"] = collection["name"]
        else:
            item["collection"] = ""

        item["formattedTitle"] = formatted_title(item)
        item["freshness"] = freshness(item, now)
        if slim:
            # ezBEQ 2.12.4's UI never reads catalogue_url or underlying, and
            # digest is already preserved as id. Keep timestamps (What's New),
            # images, and all other display metadata. Summaries load online.
            for field in ("overview", "catalogue_url", "underlying", "digest"):
                item.pop(field, None)
        processed.append(item)

    return json.dumps(processed, separators=(",", ":")).encode("utf-8")


def unique_sorted_strings(values) -> list[str]:
    seen: dict[str, str] = {}
    for value in values:
        if value is None:
            continue
        if not isinstance(value, str):
            value = str(value)
        value = value.strip()
        if not value:
            continue
        seen.setdefault(value.casefold(), value)
    return [seen[key] for key in sorted(seen)]


def build_filter_metadata(entries: list[dict]) -> dict[str, list]:
    authors = unique_sorted_strings(entry.get("author") for entry in entries)
    years = [
        year
        for year in unique_sorted_strings(entry.get("year") for entry in entries)
        if re.fullmatch(r"\d{4}", year)
    ]
    audio_types = unique_sorted_strings(
        audio_type
        for entry in entries
        for audio_type in (entry.get("audioTypes") or [])
    )
    content_types = unique_sorted_strings(
        (entry.get("content_type") or entry.get("contentType")) for entry in entries
    )
    languages = unique_sorted_strings(entry.get("language") for entry in entries)
    return {
        "authors": authors,
        "filterauthors": unique_sorted_strings(entry.get("filterAuthor") for entry in entries),
        "years": sorted((int(year) for year in years), reverse=True),
        "audiotypes": audio_types,
        "contenttypes": content_types,
        "languages": languages,
    }


def formatted_title(entry: dict) -> str:
    """Match CatalogueEntry TV formatting, including consecutive episode ranges."""
    title = entry.get("title") or ""
    if entry.get("content_type") != "TV":
        return title
    season = f"S{entry['season']}" if entry.get("season") else ""
    episodes = str(entry.get("episode") or "").split(",")
    groups = []
    if episodes != [""]:
        start = end = episodes[0]
        for episode in episodes[1:]:
            if int(episode) != int(end) + 1:
                groups.append(start if start == end else f"{start}-{end}")
                start = episode
            end = episode
        groups.append(start if start == end else f"{start}-{end}")
    return title + " " + season + ("E" + ", ".join(groups) if groups else "")


def freshness(entry: dict, now: float | None = None) -> str:
    """Single freshness calculation for listing and feed, like __fetch_entries.

    Both artifacts describe the same catalogue snapshot, at bundle build time.
    Missing timestamps become Unknown, as in upstream catalogue.py.
    """
    if not entry.get("created_at") or not entry.get("updated_at"):
        return "Unknown"
    cutoff = (now if now is not None else datetime.now(timezone.utc).timestamp()) - TWO_WEEKS
    if entry["created_at"] >= cutoff:
        return "Fresh"
    return "Updated" if entry["updated_at"] >= cutoff else "Stale"


def load_optimised() -> dict:
    """Use only the published, hash-verified additive-feedback float32 profile."""
    try:
        index = fetch_json(DEVICE_BASE_URL + "index.json", timeout=30)
        if index.get("schema_version") != 1:
            raise ValueError("unsupported device index schema")
        profile = next(p for p in index["profiles"] if p["id"] == "float32-96k")
        if (profile["rate"], profile["storage"], profile["transport"]) != (SAMPLE_RATE, "float32", "float32"):
            raise ValueError("device index profile mismatch")
        raw = fetch_bytes(DEVICE_BASE_URL + profile["file"], timeout=120)
        if hashlib.sha256(raw).hexdigest() != profile["sha256"]:
            raise ValueError("device profile sha256 mismatch")
        data = json.loads(raw)
        if (data.get("schema_version"), data.get("profile"), data.get("rate"), data.get("loading_model")) != (
            1, "float32-96k", SAMPLE_RATE, "additive-feedback-decimal17-v1"
        ):
            raise ValueError("device profile header mismatch")
        return data["entries"]
    except Exception as exc:
        print(f"  WARNING: optimised profile unavailable, using authored coefficients: {exc}")
        return {}


def entry_coefficients(entry: dict, fs: int, optimised: dict) -> tuple[list[list[float]], bool]:
    filters = entry.get("filters", [])
    if len(filters) > MAX_FILTERS:
        raise ValueError(f"entry has {len(filters)} filters, maximum is {MAX_FILTERS}")
    published = optimised.get(entry_digest(entry))
    if fs == SAMPLE_RATE and isinstance(published, list) and published and len(published) == len(filters):
        try:
            values = [[float(x) for x in bq["b"] + bq["a"]] for bq in published]
            if all(len(bq["b"]) == 3 and len(bq["a"]) == 2 for bq in published) and all(
                len(c) == 5 and all(math.isfinite(x) for x in c) for c in values
            ):
                for coefficients in values:
                    struct.pack("<5f", *coefficients)  # Must fit the device's float32 storage.
                # No feedback sign conversion: the profile already uses additive feedback.
                return values, True
        except (KeyError, TypeError, ValueError, OverflowError, struct.error):
            pass
    return [get_biquad_coefficients(filt, fs) for filt in filters], False


def build_binary_record(entry: dict, fs: int, optimised: dict | None = None) -> bytes:
    coefficients, is_optimised = entry_coefficients(entry, fs, optimised or {})
    mv = float(entry.get("mv") or 0)
    if not math.isfinite(mv) or abs(mv) > 40:
        raise ValueError("invalid MV adjustment")
    data = struct.pack("<Bf", len(coefficients) | (0x80 if is_optimised else 0), mv)
    for coeffs in coefficients:
        if not all(math.isfinite(x) for x in coeffs):
            raise ValueError("non-finite coefficient")
        data += struct.pack("<5f", *coeffs)
    return data


def build_recent_changes(entries: list[dict], now: int) -> list[dict]:
    # The stock UI asks for upstream's default window (two weeks). Keep a
    # generous RECENT_CHANGES_WINDOW_SEC so ?since= still works offline,
    # without storing every entry ever published.
    fields = ("id", "formattedTitle", "year", "author", "content_type", "contentType",
              "audioTypes", "language", "created_at", "updated_at", "freshness")
    result = [{k: e[k] for k in fields if e.get(k)} for e in entries]
    changed = lambda e: max(e.get("created_at", 0), e.get("updated_at", 0))
    result = [e for e in result if changed(e) >= now - RECENT_CHANGES_WINDOW_SEC]
    return sorted(result, key=changed, reverse=True)


def download_ui(output_dir: Path) -> tuple[str, str, dict[str, dict]]:
    """Download the ezBEQ React UI from the PyPI wheel.

    Returns (ui_version, ui_build_stamp, {relative_path: {size, sha256}}).
    """
    # UI upgrades are deliberate; catalogue data remains automatic.
    print("Fetching ezBEQ UI from PyPI...")
    pypi = fetch_json(PYPI_JSON_URL, timeout=15)
    ui_version = pypi["info"]["version"]
    if ui_version != UI_VERSION:
        raise RuntimeError("PyPI returned an unexpected ezBEQ version")

    # Find the .whl URL (wheels are zip files)
    whl_url = None
    ui_build_stamp = None
    for url_info in pypi["urls"]:
        if url_info["filename"].endswith(".whl"):
            whl_url = url_info["url"]
            upload_iso = url_info.get("upload_time_iso_8601") or url_info.get("upload_time")
            if upload_iso:
                ui_build_stamp = iso_to_build_stamp(upload_iso)
            break
    if not whl_url:
        raise RuntimeError("No wheel found on PyPI for ezbeq")
    if not ui_build_stamp:
        raise RuntimeError("No upload timestamp found on PyPI for ezbeq wheel")

    print(f"  version: {ui_version}")
    print(f"  wheel: {whl_url.split('/')[-1]}")

    # Download wheel to temp file and extract ui/ files
    ui_dir = output_dir / "ui"
    if ui_dir.exists():
        shutil.rmtree(ui_dir)
    ui_dir.mkdir()

    with tempfile.NamedTemporaryFile(suffix=".whl") as tmp:
        wheel = fetch_bytes(whl_url, timeout=60)
        if hashlib.sha256(wheel).hexdigest() != UI_WHEEL_SHA256:
            raise RuntimeError("ezBEQ wheel sha256 mismatch")
        tmp.write(wheel)
        tmp.flush()

        with zipfile.ZipFile(tmp.name) as zf:
            licenses = [n for n in zf.namelist() if ".dist-info/" in n
                        and n.rsplit("/", 1)[-1].upper() in ("LICENSE", "LICENSE.TXT")]
            if len(licenses) != 1:
                raise RuntimeError("expected ezBEQ LICENSE in pinned wheel")
            license_dir = output_dir / "licenses"
            license_dir.mkdir(exist_ok=True)
            (license_dir / "ezbeq-LICENSE.txt").write_bytes(zf.read(licenses[0]))
            ui_files = [n for n in zf.namelist() if n.startswith("ezbeq/ui/") and not n.endswith("/")]
            for name in ui_files:
                # Strip "ezbeq/ui/" prefix -> relative path under ui/
                rel = name[len("ezbeq/ui/"):]
                # Zip slip protection: reject paths with traversal or absolute components
                if ".." in rel.split("/") or rel.startswith("/"):
                    print(f"  WARNING: skipping suspicious zip entry: {name}")
                    continue
                # Firmware serves originals and modern browsers use WOFF2.
                # Omit unused precompressed siblings and legacy WOFF alternatives.
                if rel.endswith((".gz", ".br", ".woff")):
                    continue
                dest = ui_dir / rel
                if not str(dest.resolve()).startswith(str(ui_dir.resolve())):
                    print(f"  WARNING: skipping zip entry escaping ui/: {name}")
                    continue
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(zf.read(name))

    # Inject the JS shim into index.html
    for path in ui_dir.rglob("*"):
        if path.is_file() and path.suffix in (".gz", ".br", ".woff"):
            path.unlink()
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
    return ui_version, ui_build_stamp, files


def inject_shim(ui_dir: Path):
    """Inject the ezbeq-shim.js script tag into index.html and write the shim file."""
    index_path = ui_dir / "index.html"
    if not index_path.exists():
        print("  WARNING: no index.html found, skipping shim injection")
        return

    # Write the shim JS file
    shim_path = ui_dir / "ezbeq-shim.js"
    shim_js = SHIM_PATH.read_text(encoding="utf-8")
    shim_path.write_text(shim_js, encoding="utf-8")
    print(f"  wrote ezbeq-shim.js ({len(shim_js):,} bytes)")
    shim_version = hashlib.sha256(shim_js.encode("utf-8")).hexdigest()[:12]

    # Inject script tag before </head>
    html = index_path.read_text(encoding="utf-8")
    if "ezbeq-shim.js" in html:
        return  # Already injected
    script_tag = f'<script src="/ezbeq-shim.js?v={shim_version}"></script>\n</head>'
    new_html = html.replace("</head>", script_tag)
    if new_html == html:
        print("  WARNING: </head> not found in index.html, shim NOT injected")
        return
    index_path.write_text(new_html, encoding="utf-8")
    print("  injected shim into index.html")



def main():
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 and sys.argv[1] == "--output-dir" else Path(".")

    output_dir.mkdir(parents=True, exist_ok=True)

    # Download catalogue
    print(f"Downloading catalogue from {CATALOGUE_URL}...")
    raw = fetch_bytes(CATALOGUE_URL, timeout=60)
    raw_entries = json.loads(raw)
    # Upstream catalogue merging keeps the first full digest. Repeated full
    # digests are legitimate; distinct full digests sharing a prefix are not.
    db = deduplicate_entries(raw_entries)
    print(f"  {len(db)} entries ({len(raw_entries) - len(db)} repeated digests omitted)")

    # Mirror the full upstream catalogue JSON to Pages for browser-side use.
    database_path = output_dir / "database.json"
    database_path.write_bytes(raw)
    print(f"  database.json: {database_path.stat().st_size:,} bytes")

    # Also publish a slimmed, gzipped browser catalogue for the ESP32 itself.
    browser_catalogue = build_browser_catalogue(db)
    browser_catalogue_gz = gzip.compress(browser_catalogue, compresslevel=9, mtime=0)
    previous_gz_size = len(gzip.compress(build_browser_catalogue(db, slim=False), compresslevel=9))
    browser_catalogue_path = output_dir / "database.json.gz"
    browser_catalogue_path.write_bytes(browser_catalogue_gz)
    print(
        "  database.json.gz: "
        f"{previous_gz_size:,} -> {browser_catalogue_path.stat().st_size:,} bytes "
        f"({len(browser_catalogue):,} bytes uncompressed)"
    )

    # Pages-only asset: never add this to manifest.files or download it to flash.
    overviews = {entry_digest(e): e.get("overview") or "" for e in db}
    overview_path = output_dir / "overviews.json.gz"
    overview_path.write_bytes(gzip.compress(json.dumps(overviews, separators=(",", ":")).encode(), mtime=0))
    print(f"  overviews.json.gz (Pages only): {overview_path.stat().st_size:,} bytes")

    built_at = int(datetime.now(timezone.utc).timestamp())
    browser_entries = json.loads(browser_catalogue)

    # Title/author for every entry, so the firmware can describe a loaded slot
    # the way upstream does. catalogue.bin records point at each row's offset;
    # only the two fields the firmware needs are stored.
    metadata_path = output_dir / "entry-metadata.jsonl"
    metadata_offsets = {}
    with metadata_path.open("wb") as index:
        for entry in browser_entries:
            metadata_offsets[entry["id"]] = index.tell()
            row = [entry.get("id"), entry.get("formattedTitle"), entry.get("author")]
            encoded = json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"
            if len(encoded) > 4096:
                raise RuntimeError("catalogue metadata row exceeds firmware read limit")
            index.write(encoded)

    # Sorted JSONL lets the firmware stream the What's New feed and stop at ?since=.
    recent_path = output_dir / "recent-changes.jsonl"
    with recent_path.open("wb") as feed:
        for entry in build_recent_changes(browser_entries, built_at):
            row = [entry.get(k) for k in ("id", "formattedTitle", "year", "author", "content_type",
                                         "audioTypes", "language", "created_at", "updated_at", "freshness")]
            feed.write(json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode() + b"\n")
    metadata_files: dict[str, dict[str, str | int]] = {
        path.name: {"size": path.stat().st_size,
                    "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
        for path in (metadata_path, recent_path)
    }
    for name, values in build_filter_metadata(db).items():
        metadata_path = output_dir / f"{name}.json"
        metadata_path.write_text(json.dumps(values, separators=(",", ":")) + "\n")
        metadata_files[f"{name}.json"] = {
            "size": metadata_path.stat().st_size,
            "sha256": hashlib.sha256(metadata_path.read_bytes()).hexdigest(),
        }
    built_at_path = output_dir / "catalogue-built-at.txt"
    built_at_path.write_text(str(built_at) + "\n")
    metadata_files[built_at_path.name] = {
        "size": built_at_path.stat().st_size,
        "sha256": hashlib.sha256(built_at_path.read_bytes()).hexdigest(),
    }
    print(f"  metadata files: {len(metadata_files)}")

    # Download version
    try:
        version = fetch_bytes(VERSION_URL, timeout=10).decode().strip()
    except Exception:
        version = hashlib.sha256(raw).hexdigest()[:12]
    print(f"  version: {version}")

    # Build binary biquad index (sorted by digest prefix for binary search)
    print("Building binary index...")
    optimised = load_optimised()
    records = []
    optimised_count = 0
    skipped = 0
    for entry in db:
        digest = entry_digest(entry)
        try:
            record = build_binary_record(entry, SAMPLE_RATE, optimised)
            record += struct.pack("<I", metadata_offsets[digest])
            optimised_count += bool(record[0] & 0x80)
            records.append((bytes.fromhex(digest[:DIGEST_PREFIX_SIZE * 2]), record))
        except Exception as e:
            skipped += 1
            if skipped <= 3:
                print(f"  WARNING: skipping entry '{entry.get('title', '?')}': {e}")

    # Sort by digest prefix for binary search
    records.sort(key=lambda r: r[0])
    if any(a[0] == b[0] for a, b in zip(records, records[1:])):
        raise RuntimeError("duplicate catalogue digest prefixes")

    bin_path = output_dir / "catalogue.bin"
    with open(bin_path, "wb") as f:
        # Header: magic(4) + entry_count(u32) + sample_rate(u32)
        f.write(BIN_MAGIC)
        f.write(struct.pack("<II", len(records), SAMPLE_RATE))
        offset = 12 + len(records) * 12
        for prefix, record in records:
            f.write(prefix + struct.pack("<I", offset))
            offset += len(record)
        for _, record in records:
            f.write(record)
    bin_size = bin_path.stat().st_size
    print(f"  catalogue.bin: {bin_size:,} bytes ({bin_size/1024/1024:.1f}MB), {len(records)} entries")
    print(f"  optimised: {optimised_count}, authored fallback: {len(records) - optimised_count}")
    if skipped:
        raise RuntimeError(f"{skipped} catalogue entries could not be represented")

    # Download ezBEQ React UI from PyPI
    catalogue_stamp = catalogue_build_stamp(version)
    repo_stamp = repo_build_stamp()
    ui_version, ui_stamp, ui_files = download_ui(output_dir)

    # Carry upstream notices with every stored/hosted copy, served by the
    # generic static handler. The wheel's LICENSE was extracted above.
    license_dir = output_dir / "licenses"
    (license_dir / "beqcatalogue-LICENSE.txt").write_bytes(fetch_bytes(CATALOGUE_LICENSE_URL))
    (license_dir / "ATTRIBUTION.txt").write_text(
        "ezBEQ UI 2.12.4 by Matt Khan (3ll3d00d): https://github.com/3ll3d00d/ezbeq\n"
        "BEQ catalogue and device profiles by Matt Khan and the BEQ community: "
        "https://github.com/3ll3d00d/beqcatalogue\n"
        "Upstream content is consumed as published; derived storage indexes and the "
        "ESP32 API adapter are maintained by minidsp-open. See the adjacent MIT licenses.\n"
    )
    for path in sorted(license_dir.iterdir()):
        metadata_files[str(path.relative_to(output_dir))] = {
            "size": path.stat().st_size, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()
        }

    # Build manifest — catalogue.bin + UI files
    files = {
        "catalogue.bin": {
            "size": bin_size,
            "sha256": hashlib.sha256(bin_path.read_bytes()).hexdigest(),
        },
        "database.json.gz": {
            "size": browser_catalogue_path.stat().st_size,
            "sha256": hashlib.sha256(browser_catalogue_path.read_bytes()).hexdigest(),
        },
    }
    files.update(metadata_files)
    files.update(ui_files)
    site_hash = hashlib.sha256(
        json.dumps(files, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()[:12]
    site_build_stamp = max(catalogue_stamp, ui_stamp, repo_stamp)
    site_version = f"{site_build_stamp}+cat{version[:12]}+ui{ui_version}+site{site_hash}"

    # version.txt is consumed by the browser-side cache layer, so it needs to
    # change when the site payload changes, not only when the upstream BEQ
    # catalogue revision changes.
    version_path = output_dir / "version.txt"
    version_path.write_text(site_version + "\n")
    files["version.txt"] = {
        "size": version_path.stat().st_size,
        "sha256": hashlib.sha256(version_path.read_bytes()).hexdigest(),
    }

    manifest = {
        "version": site_version,
        "site_build_stamp": site_build_stamp,
        "catalogue_version": version,
        "catalogue_build_stamp": catalogue_stamp,
        "ui_version": ui_version,
        "ui_build_stamp": ui_stamp,
        "repo_build_stamp": repo_stamp,
        "entry_count": len(records),
        "files": files,
    }
    manifest_path = output_dir / "manifest.json"
    manifest_text = json.dumps(manifest, separators=(",", ":")) + "\n"
    if len(manifest_text.encode()) > 8192:
        raise RuntimeError("manifest exceeds firmware's 8192-byte fetch limit")
    manifest_path.write_text(manifest_text)
    print(f"  manifest.json: {len(files)} files, version {site_version}")

    total = sum(f["size"] for f in files.values()) + manifest_path.stat().st_size
    for name, metadata in sorted(files.items()):
        print(f"  {name}: {metadata['size']:,} bytes")
    print(f"\nTotal: {total:,} bytes / {BUNDLE_BUDGET:,} byte budget")
    if total > BUNDLE_BUDGET:
        raise RuntimeError("ESP32 bundle exceeds 8.0 MB budget")


if __name__ == "__main__":
    main()
