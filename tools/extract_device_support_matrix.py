#!/usr/bin/env python3
"""Build a repeatable device support matrix from official app assets.

Inputs:
- app/MiniDSP Device Console.app/.../device_support_package/
- app/apk-extracted/assets/www/js/controllers.js
- protocol/src/device/probe.rs

Outputs:
- docs/ref-device-coverage.md
- docs/generated/device-support-matrix.json
"""

from __future__ import annotations

import json
import re
import xml.etree.ElementTree as ET
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]
PACKAGE_ROOT = REPO_ROOT / "app" / "MiniDSP Device Console.app" / "Contents" / "Resources" / "device_support_package"
APK_CONTROLLERS = REPO_ROOT / "app" / "apk-extracted" / "assets" / "www" / "js" / "controllers.js"
DESKTOP_MAIN_BUNDLE = REPO_ROOT / "app" / "app-extracted" / "build" / "src" / "index.js"
DESKTOP_USERSETTINGS_BUNDLE = REPO_ROOT / "app" / "app-extracted" / "app" / "src_app_usersettings_usersettings_module_ts.e0768996ea536a9d.js"
DESKTOP_DEVICE_TYPE_ASSET_DIR = REPO_ROOT / "app" / "app-extracted" / "app" / "assets" / "img" / "device-types"
DIRAC_BINARY = REPO_ROOT / "app" / "MiniDSP Device Console.app" / "Contents" / "Resources" / "dirac" / "minidsp_dirac"
PROBE_RS = REPO_ROOT / "protocol" / "src" / "device" / "probe.rs"
LEGACY_CONFIG_ROOT = REPO_ROOT / "third_party" / "legacy-configs"
OUTPUT_MD = REPO_ROOT / "docs" / "ref-device-coverage.md"
OUTPUT_JSON = REPO_ROOT / "docs" / "generated" / "device-support-matrix.json"
OUTPUT_RS = REPO_ROOT / "protocol" / "src" / "device" / "generated_official.rs"


@dataclass
class RustProfile:
    name: str
    hw_id: int
    dsp_version: int
    tier: str


@dataclass
class RustProfileTemplate:
    fn_name: str
    name: str
    hw_expr: str
    dsp_version: int
    tier: str


@dataclass
class DesktopFallbackModel:
    dsp_id: int
    models: list[str]


LEGACY_CONFIGS: list[dict[str, Any]] = [
    {"key": "m4x10hd", "name": "4x10 HD", "hw_id": 1, "dsp_version": None, "dialect": "old"},
    {"key": "m10x10hd", "name": "10x10 HD", "hw_id": 1, "dsp_version": 51, "dialect": "old"},
    {"key": "m2x4", "name": "miniDSP 2x4", "hw_id": 2, "dsp_version": 22, "dialect": "old"},
    {"key": "nanodigi2x8", "name": "NanoDigi 2x8", "hw_id": 2, "dsp_version": 54, "dialect": "old"},
    {"key": "msharc4x8", "name": "miniSHARC 4x8", "hw_id": 4, "dsp_version": None, "dialect": "new"},
    {"key": "ddrc88bm", "name": "DDRC-88BM", "hw_id": 6, "dsp_version": 95, "dialect": "new"},
    {"key": "c8x12v2", "name": "C-DSP 8x12 v2", "hw_id": 11, "dsp_version": 97, "dialect": "new"},
]

USB_HINT_RESOLUTION: dict[str, dict[str, str]] = {
    "all PIC base product without DIRAC": {
        "status": "covered",
        "exact_runtime_known": "false",
        "resolution": "legacy-family USB surface is accepted; runtime identity still comes from hardware probing",
    },
    "2x4 HD / Flex": {
        "status": "covered",
        "exact_runtime_known": "true",
        "resolution": "covered by existing 2x4 HD / Flex runtime profiles after hw_id/dsp_version probing",
    },
    "DDRC-24 / Flex (DL)": {
        "status": "covered",
        "exact_runtime_known": "true",
        "resolution": "covered by existing DDRC-24 / Flex (DL) runtime profiles after hw_id/dsp_version probing",
    },
    "SHD": {
        "status": "covered",
        "exact_runtime_known": "true",
        "resolution": "covered by existing SHD-family runtime profiles after hw_id/dsp_version probing",
    },
    "miniDSP Flex Eight": {
        "status": "covered",
        "exact_runtime_known": "true",
        "resolution": "covered by existing Flex Eight runtime profile after hw_id/dsp_version probing",
    },
    "miniDSP Flex Eight (DL)": {
        "status": "covered",
        "exact_runtime_known": "true",
        "resolution": "covered by existing Flex Eight (DL) runtime profile after hw_id/dsp_version probing",
    },
    "miniDSP Flex HT / Flex HTx": {
        "status": "covered",
        "exact_runtime_known": "true",
        "resolution": "covered by existing Flex HT / Flex HTx runtime profiles after hw_id/dsp_version probing",
    },
    "HA-DSP": {
        "status": "alias_only",
        "exact_runtime_known": "false",
        "resolution": "official assets currently expose only the USB PID label; no exact or family-level runtime mapping has been verified yet",
    },
    "DDRC-22": {
        "status": "alias_only",
        "exact_runtime_known": "false",
        "resolution": "official assets currently expose only the USB PID label; no exact or family-level runtime mapping has been verified yet",
    },
    "DDRC-88": {
        "status": "family_candidate",
        "exact_runtime_known": "false",
        "resolution": "name strongly suggests the same 8x8 Dirac legacy family as the cached DDRC-88BM profile, but the extracted assets still do not expose an exact hw_id/dsp_id mapping",
        "candidate_runtime_profiles": "DDRC-88BM",
        "candidate_source_basis": "official USB PID label + source-backed cached legacy DDRC-88BM family",
    },
    "CDSP-8x12/DL": {
        "status": "family_candidate",
        "exact_runtime_known": "false",
        "resolution": "name strongly suggests the same 8x12 custom-install family as the cached C-DSP 8x12 v2 profile, but the extracted assets still do not expose an exact hw_id/dsp_id mapping",
        "candidate_runtime_profiles": "C-DSP 8x12 v2",
        "candidate_source_basis": "official USB PID label + source-backed cached legacy C-DSP 8x12 v2 family",
    },
    "Harmony DSP": {
        "status": "alias_only",
        "exact_runtime_known": "false",
        "resolution": "official assets currently expose only the USB PID label; no exact or family-level runtime mapping has been verified yet",
    },
}

MINIDSP_RS_REFERENCE_PROFILES: list[dict[str, Any]] = [
    {"hw_id": 1, "dsp_version": 0, "name": "4x10 HD"},
    {"hw_id": 1, "dsp_version": 51, "name": "10x10 HD"},
    {"hw_id": 2, "dsp_version": 22, "name": "miniDSP 2x4"},
    {"hw_id": 2, "dsp_version": 54, "name": "NanoDigi 2x8"},
    {"hw_id": 4, "dsp_version": 0, "name": "miniSHARC 4x8"},
    {"hw_id": 6, "dsp_version": 95, "name": "DDRC-88BM"},
    {"hw_id": 10, "dsp_version": 100, "name": "2x4 HD"},
    {"hw_id": 10, "dsp_version": 101, "name": "DDRC-24"},
    {"hw_id": 11, "dsp_version": 97, "name": "C-DSP 8x12 v2"},
    {"hw_id": 14, "dsp_version": 105, "name": "SHD"},
    {"hw_id": 27, "dsp_version": 100, "name": "Flex"},
    {"hw_id": 27, "dsp_version": 101, "name": "Flex (DL)"},
    {"hw_id": 32, "dsp_version": 113, "name": "Flex HTx"},
]


def load_xml(path: Path) -> ET.Element:
    return ET.parse(path).getroot()


def text_or_none(parent: ET.Element, tag: str) -> str | None:
    node = parent.find(tag)
    if node is None or node.text is None:
        return None
    return node.text.strip()


def parse_display(hw_root: ET.Element) -> dict[str, Any]:
    dim = hw_root.find("./DISPLAY/DIM_CTRL")
    if dim is None:
        return {"has_display": False}
    idle = dim.find("IDLE_SECS")
    bright = dim.find("BRIGHTNESS_LEVEL")
    return {
        "has_display": True,
        "idle_secs_default": idle.text.strip() if idle is not None and idle.text else None,
        "idle_secs_min": idle.get("min") if idle is not None else None,
        "idle_secs_max": idle.get("max") if idle is not None else None,
        "brightness_default": bright.text.strip() if bright is not None and bright.text else None,
        "brightness_min": bright.get("min") if bright is not None else None,
        "brightness_max": bright.get("max") if bright is not None else None,
    }


def parse_hw_info(path: Path) -> dict[str, Any]:
    root = load_xml(path)
    hw = root.find("HW")
    if hw is None:
        raise ValueError(f"missing HW node in {path}")
    audio_sources = hw.find("AUDIO_IN_SOURCES")
    sources = []
    if audio_sources is not None:
        for src in audio_sources.findall("IN_SOURCE"):
            sources.append(
                {
                    "idx": int(src.get("idx")),
                    "name": src.get("name"),
                }
            )
    dsp_min_versions = {}
    for node in hw.findall("DSP_MIN_VERSION"):
        dsp_min_versions[int(node.get("id"))] = node.text.strip() if node.text else None
    info = {
        "hw_id": int(hw.get("id")),
        "hw_name": hw.get("name"),
        "ctl_type": text_or_none(hw, "CTL_TYPE"),
        "dsp_type": text_or_none(hw, "DSP_TYPE"),
        "storage": text_or_none(hw, "STORAGE"),
        "xmos_fw_version": text_or_none(hw, "XMOS_FW_VERSION"),
        "xmos_fw_file": text_or_none(hw, "XMOS_FW_FILE"),
        "xmos_min_version": text_or_none(hw, "XMOS_MIN_VERSION"),
        "xmos_hid_upgrade_version": text_or_none(hw, "XMOS_HID_UPGRADE_VERSION"),
        "rew_supported": hw.find("REW_SUPPORTED") is not None,
        "has_bluetooth_module": hw.find("HAS_BLUETOOTH_MODULE") is not None,
        "has_dre_option": hw.find("HAS_DRE_OPTION") is not None,
        "audio_src_selectable": audio_sources is not None and audio_sources.get("selectable") == "true",
        "audio_src_has_voloffsets": audio_sources is not None and audio_sources.get("voloffsets") == "true",
        "audio_sources": sources,
        "dsp_min_versions": dsp_min_versions,
    }
    info.update(parse_display(hw))
    return info


def parse_dsp_info(path: Path) -> dict[str, Any]:
    root = load_xml(path)
    dsp = root.find("DSP")
    if dsp is None:
        raise ValueError(f"missing DSP node in {path}")
    fw_convert = dsp.find("DSP_FW_CONVERT")
    return {
        "dsp_id": int(dsp.get("id")),
        "dsp_name": dsp.get("name"),
        "adobe_air_app": text_or_none(dsp, "ADOBEAIR_APP"),
        "cmd_version": text_or_none(dsp, "CMD_VERSION"),
        "dsp_fw_version": text_or_none(dsp, "DSP_FW_VERSION"),
        "dsp_fw_file": text_or_none(dsp, "DSP_FW_FILE"),
        "fw_convert": {
            "intermediate_dsp_id": fw_convert.get("intermediateDspId"),
            "final_dsp_id": fw_convert.get("finalDspId"),
        }
        if fw_convert is not None
        else None,
    }


def parse_dsp_default_params(dsp_id: int) -> dict[str, Any]:
    path = PACKAGE_ROOT / "DSP_Structures" / str(dsp_id) / "DSP_default_parameters.xml"
    root = load_xml(path)
    params = root.find("params")
    range_names = []
    rep_counts: dict[str, int] = {}
    rep_examples: dict[str, list[dict[str, Any]]] = {}
    items: list[dict[str, Any]] = []
    filters: list[dict[str, Any]] = []
    filter_counts: dict[str, int] = {}
    if params is not None:
        for item in params.findall("item"):
            rep = item.get("rep", "unknown")
            item_data = {
                "name": item.get("name"),
                "idx": int(item.get("idx")),
                "rep": rep,
                "range": item.get("range"),
                "default_hex": (item.text or "").strip(),
            }
            items.append(item_data)
            rep_counts[rep] = rep_counts.get(rep, 0) + 1
            if rep not in rep_examples:
                rep_examples[rep] = []
            if len(rep_examples[rep]) < 5:
                rep_examples[rep].append(item_data)
    for filter_node in root.findall(".//filter"):
        filter_type = filter_node.get("type", "unknown")
        filter_data = {
            "type": filter_type,
            "channel": int(filter_node.get("chn")) if filter_node.get("chn") is not None else None,
            "band": int(filter_node.get("band")) if filter_node.get("band") is not None else None,
            "idx": int(filter_node.get("idx")),
        }
        filters.append(filter_data)
        filter_counts[filter_type] = filter_counts.get(filter_type, 0) + 1
    params_range = root.find("params_range")
    if params_range is not None:
        for child in list(params_range):
            range_names.append(child.tag)
    return {
        "dsp_id": dsp_id,
        "rep_counts": rep_counts,
        "rep_examples": rep_examples,
        "range_names": range_names,
        "items": items,
        "filters": filters,
        "filter_counts": filter_counts,
    }


def parse_top_level_modern_devices() -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    top_path = PACKAGE_ROOT / "info.xml"
    raw = top_path.read_text()
    root = load_xml(top_path)
    devices = []
    usb_entries = []

    for match in re.finditer(
        r'<DEVICE vid="(?P<vid>0x[0-9A-Fa-f]+)" pid="(?P<pid>0x[0-9A-Fa-f]+)"\s*/>\s*<!--\s*(?P<label>.*?)\s*-->',
        raw,
    ):
        usb_entries.append(
            {
                "vid": match.group("vid"),
                "pid": match.group("pid"),
                "label": match.group("label").strip(),
            }
        )

    for hw in root.findall("./DEVICES/HW"):
        hw_id = int(hw.get("id"))
        hw_info = parse_hw_info(PACKAGE_ROOT / "HW" / str(hw_id) / "info.xml")
        for dsp in hw.findall("./DSP_STRUCTURES/DSP"):
            dsp_id = int(dsp.get("id"))
            dsp_info = parse_dsp_info(PACKAGE_ROOT / "DSP_Structures" / str(dsp_id) / "info.xml")
            dsp_defaults = parse_dsp_default_params(dsp_id)
            macos_name = text_or_none(dsp, "./USB_AUDIO_NAMES/MACOS")
            windows_name = text_or_none(dsp, "./USB_AUDIO_NAMES/WINDOWS")
            devices.append(
                {
                    "hw_id": hw_id,
                    "hw_name": hw.get("name"),
                    "dsp_version": dsp_id,
                    "product_name": dsp.get("name") or hw.get("name"),
                    "macos_usb_audio_name": macos_name,
                    "windows_usb_audio_name": windows_name,
                    "hw_info": hw_info,
                    "dsp_info": dsp_info,
                    "dsp_defaults": dsp_defaults,
                }
            )
    return devices, usb_entries


def parse_package_inventory() -> dict[str, Any]:
    hw_dirs = sorted(
        int(path.name)
        for path in (PACKAGE_ROOT / "HW").iterdir()
        if path.is_dir() and path.name.isdigit()
    )
    dsp_dirs = sorted(
        int(path.name)
        for path in (PACKAGE_ROOT / "DSP_Structures").iterdir()
        if path.is_dir() and path.name.isdigit()
    )
    return {
        "exact_hw_ids": hw_dirs,
        "exact_dsp_ids": dsp_dirs,
    }


def parse_device_type_assets() -> dict[str, Any]:
    pairs = []
    extra_assets = []
    for path in sorted(DESKTOP_DEVICE_TYPE_ASSET_DIR.iterdir()):
        if not path.is_file():
            continue
        match = re.fullmatch(r"product-(\d+)-(\d+)\.png", path.name)
        if match:
            pairs.append(
                {
                    "filename": path.name,
                    "hw_id": int(match.group(1)),
                    "dsp_version": int(match.group(2)),
                }
            )
        else:
            extra_assets.append(path.name)
    return {
        "product_assets": pairs,
        "extra_assets": extra_assets,
    }


def parse_dirac_binary_hints() -> dict[str, Any]:
    raw = DIRAC_BINARY.read_bytes()
    strings = {
        match.group(0).decode("utf-8", errors="ignore")
        for match in re.finditer(rb"[\x20-\x7e]{4,}", raw)
    }
    return {
        "contains_harmony_dsp_8x12": "Harmony DSP 8x12" in strings,
        "contains_padsp_8x12_dl": "PADSP-8x12-DL" in strings,
        "contains_assume_dsp_113": "Failed to get dsp id, assume it is 113" in strings,
        "contains_dsp_id_115": "dsp id is 115" in strings,
        "notes": (
            "The packaged Dirac helper binary includes `Harmony DSP 8x12`, "
            "`Failed to get dsp id, assume it is 113`, `dsp id is 115`, and "
            "`PADSP-8x12-DL`. That is stronger official evidence than the top-level XML "
            "for Harmony / 8x12-DL family hints, but it still does not expose an exact "
            "`hw_id` mapping."
        ),
    }


def parse_legacy_configs() -> list[dict[str, Any]]:
    rows = []
    for config in LEGACY_CONFIGS:
        path = LEGACY_CONFIG_ROOT / config["key"] / "config.xml"
        root = load_xml(path)

        labels = []
        for label in root.findall("label"):
            labels.append(
                {
                    "name": label.get("name"),
                    "text": label.get("text"),
                }
            )

        items = []
        max_addr = None
        for item in root.findall("item"):
            addr = int(item.get("addr"))
            max_addr = addr if max_addr is None else max(max_addr, addr)
            items.append(
                {
                    "name": item.get("name"),
                    "addr": addr,
                }
            )

        filters = []
        filter_type_counts: dict[str, int] = {}
        for filter_node in root.findall("filter"):
            filter_type = filter_node.findtext("type", default="unknown")
            filter_type_counts[filter_type] = filter_type_counts.get(filter_type, 0) + 1
            filters.append(
                {
                    "name": filter_node.get("name"),
                    "addr": int(filter_node.get("addr")) if filter_node.get("addr") is not None else None,
                    "type": filter_type,
                }
            )

        fir_blocks = []
        for fir in root.findall("fir"):
            fir_blocks.append(
                {
                    "name": fir.get("name"),
                    "addr": int(fir.get("addr")) if fir.get("addr") is not None else None,
                    "count": int(fir.find("para").get("count")) if fir.find("para") is not None and fir.find("para").get("count") is not None else None,
                    "rows": int(fir.find("para").get("rows")) if fir.find("para") is not None and fir.find("para").get("rows") is not None else None,
                }
            )

        input_label_count = sum(
            1 for label in labels if label["name"] and re.fullmatch(r"Input\d+", label["name"])
        )
        output_label_count = sum(
            1 for label in labels if label["name"] and re.fullmatch(r"Output\d+", label["name"])
        )
        unique_input_label_text_count = len(
            {
                label["text"]
                for label in labels
                if label["name"] and label["text"] and re.fullmatch(r"Input\d+", label["name"])
            }
        )
        unique_output_label_text_count = len(
            {
                label["text"]
                for label in labels
                if label["name"] and label["text"] and re.fullmatch(r"Output\d+", label["name"])
            }
        )

        item_names = [item["name"] for item in items if item["name"]]
        meter_input_ids = sorted(
            {
                int(match.group(1))
                for name in item_names
                if (match := re.match(r"Meter_In_(\d+)$", name))
            }
        )
        meter_output_ids = sorted(
            {
                int(match.group(1))
                for name in item_names
                if (match := re.match(r"Meter_Out_(\d+)$", name))
            }
        )
        meter_compressor_ids = sorted(
            {
                int(match.group(1))
                for name in item_names
                if (match := re.match(r"Meter_Comp_(\d+)$", name))
            }
        )
        compressor_channel_ids = sorted(
            {
                int(match.group(1))
                for name in item_names
                if (match := re.match(r"COMP_(\d+)_0_status$", name))
            }
        )
        peq_channel_ids = sorted(
            {
                int(match.group(1))
                for flt in filters
                if flt["name"] and flt["type"] == "PK" and (match := re.match(r"PEQ_(\d+)_\d+$", flt["name"]))
            }
        )
        peq_band_counts: dict[int, int] = {}
        for flt in filters:
            if flt["name"] and flt["type"] == "PK":
                match = re.match(r"PEQ_(\d+)_(\d+)$", flt["name"])
                if match:
                    channel = int(match.group(1))
                    peq_band_counts[channel] = peq_band_counts.get(channel, 0) + 1

        inferred_input_channels = 0
        inferred_output_channels = 0

        legacy_peq_ids = sorted(
            {
                int(match.group(1))
                for flt in filters
                if flt["name"] and (match := re.match(r"PEQ_(\d+)_(\d+)$", flt["name"]))
            }
        )
        if legacy_peq_ids:
            inferred_input_channels = len({channel for channel in legacy_peq_ids if channel >= 11})
            inferred_output_channels = len({channel for channel in legacy_peq_ids if channel < 11})

        eq_stage_pairs = sorted(
            {
                (int(match.group(1)), int(match.group(2)))
                for flt in filters
                if flt["name"] and (match := re.match(r"EQ_ch(\d+)_(\d+)_(\d+)$", flt["name"]))
            }
        )
        if eq_stage_pairs:
            inferred_input_channels = max(
                inferred_input_channels,
                len({channel for channel, stage in eq_stage_pairs if stage == 1}),
            )
            inferred_output_channels = max(
                inferred_output_channels,
                len({(channel, stage) for channel, stage in eq_stage_pairs if stage in (2, 3)}),
            )

        rows.append(
            {
                **config,
                "path": str(path.relative_to(REPO_ROOT)),
                "config_version": root.get("version"),
                "declared_dsp_version": int(root.findtext("dspversion")) if root.findtext("dspversion") else None,
                "label_count": len(labels),
                "labels": labels,
                "item_count": len(items),
                "filter_count": len(filters),
                "filter_type_counts": filter_type_counts,
                "max_addr": max_addr,
                "items": items,
                "filters": filters,
                "fir_blocks": fir_blocks,
                "input_label_count": input_label_count,
                "output_label_count": output_label_count,
                "unique_input_label_text_count": unique_input_label_text_count,
                "unique_output_label_text_count": unique_output_label_text_count,
                "meter_input_count": len(meter_input_ids),
                "meter_output_count": len(meter_output_ids),
                "meter_compressor_count": len(meter_compressor_ids),
                "compressor_count": len(compressor_channel_ids),
                "fir_count": len(fir_blocks),
                "peq_channel_count": len(peq_channel_ids),
                "max_peq_bands_per_channel": max(peq_band_counts.values(), default=0),
                "inferred_input_channels": inferred_input_channels,
                "inferred_output_channels": inferred_output_channels,
            }
        )
    return rows


def parse_js_source_array(array_body: str) -> list[dict[str, Any]]:
    entries = []
    for name, value in re.findall(r"\{name:'([^']+)',\s*value:([^}]+)\}", array_body):
        raw = value.strip().strip(",")
        if raw.startswith('"') or raw.startswith("'"):
            parsed: Any = raw.strip("\"'")
        else:
            parsed = int(raw, 0)
        entries.append({"name": name, "value": parsed})
    return entries


def parse_android_hints() -> dict[str, Any]:
    text = APK_CONTROLLERS.read_text()

    dirac_match = re.search(r"dirac_dsp_version\s*=\s*\[([^\]]+)\]", text)
    dirac_versions = []
    if dirac_match:
        dirac_versions = [int(v.strip()) for v in dirac_match.group(1).split(",") if v.strip()]

    groups = []
    simple_patterns = [
        ("2,11", r"if\(hw_id == 2 \|\| hw_id == 11\)\s*\{\s*return \[(.*?)\];"),
        ("14", r"else if\(hw_id == 14\)\s*\{\s*return \[(.*?)\];"),
        ("17,18", r"else if\(hw_id == 17 \|\| hw_id == 18\)\s*\{\s*return \[(.*?)\];"),
        ("27", r"else if\(hw_id == 27\)\s*\{\s*return \[(.*?)\];"),
        ("30", r"else if\(hw_id == 30\)\s*\{\s*return \[(.*?)\];"),
        ("31", r"else if\(hw_id == 31\)\s*\{\s*return \[(.*?)\];"),
        ("32", r"else if\(hw_id == 32\)\s*\{\s*return \[(.*?)\];"),
    ]
    for key, pattern in simple_patterns:
        match = re.search(pattern, text, re.S)
        if match:
            groups.append({"hw_ids": [int(v) for v in key.split(",")], "sources": parse_js_source_array(match.group(1))})

    hw145_match = re.search(
        r"else if\(hw_id == 1 \|\| hw_id == 4 \|\| hw_id == 5\)\s*\{\s*if\(input_source > 2\).*?return \[(.*?)\];\s*\}\s*else\s*\{\s*return \[(.*?)\];",
        text,
        re.S,
    )
    if hw145_match:
        groups.append(
            {
                "hw_ids": [1, 4, 5],
                "variant": "input_source>2",
                "sources": parse_js_source_array(hw145_match.group(1)),
            }
        )
        groups.append(
            {
                "hw_ids": [1, 4, 5],
                "variant": "input_source<=2",
                "sources": parse_js_source_array(hw145_match.group(2)),
            }
        )

    hw10_match = re.search(
        r"else if\(hw_id == 10\)\s*\{\s*if\(dsp_version == 100 \|\| dsp_version == 101\)\s*\{\s*return \[(.*?)\];\s*\}\s*else.*?return \[(.*?)\];",
        text,
        re.S,
    )
    if hw10_match:
        groups.append(
            {
                "hw_ids": [10],
                "variant": "dsp_version=100|101",
                "sources": parse_js_source_array(hw10_match.group(1)),
            }
        )
        groups.append(
            {
                "hw_ids": [10],
                "variant": "legacy_other_dsp",
                "sources": parse_js_source_array(hw10_match.group(2)),
            }
        )

    select_index_offset = []
    select_match = re.search(r"if\(\(hw_id == 30\) \|\| \(hw_id == 31\)\)\s*\{\s*return src_no - 1;", text)
    if select_match:
        select_index_offset = [30, 31]

    return {
        "dirac_dsp_versions": dirac_versions,
        "source_groups": groups,
        "select_index_offset_hw_ids": select_index_offset,
    }


def parse_desktop_fallback_hints() -> dict[str, Any]:
    main_text = DESKTOP_MAIN_BUNDLE.read_text()
    settings_text = DESKTOP_USERSETTINGS_BUNDLE.read_text()

    handler_name = "get-dspid-fallback-models"
    return {
        "handler_name": handler_name,
        "main_handler_present": handler_name in main_text,
        "ui_service_present": "getDspIdFallbackModels" in settings_text,
        "ui_setting_key_present": "fallback-dspId-value" in settings_text,
        "ui_prompt_present": "Select a miniDSP Model" in settings_text,
        "all_supported_devices_exports_device_id_list": "allSupportedDevices=()=>deviceSupportPkg['deviceIdList']" in main_text,
        "device_models_by_dsp_ids_present": "createModelsByDspIds" in main_text,
        "exact_model_list_extracted": True,
        "notes": (
            "The extracted desktop app builds the fallback-model picker from "
            "`deviceSupportPkg.deviceIdList`, grouped by `dspId` into `deviceModelsByDspIds`, "
            "then serves that through the `get-dspid-fallback-models` IPC handler. "
            "So the fallback picker is not an independent runtime-identity catalog and does not "
            "resolve the USB-only alias labels that are absent from `deviceIdList`."
        ),
    }


def parse_rust_profiles() -> list[RustProfile]:
    text = PROBE_RS.read_text()
    fn_pattern = re.compile(
        r"fn\s+(profile_[a-z0-9_]+)\([^)]*\)\s*->\s*DeviceProfile\s*\{(?P<body>.*?)\n\}",
        re.S,
    )
    templates: dict[str, RustProfileTemplate] = {}
    for match in fn_pattern.finditer(text):
        fn_name = match.group(1)
        body = match.group("body")
        name_match = re.search(r'name:\s*"([^"]+)"', body)
        hw_match = re.search(r"(?:hw_id:\s*([^,]+)|\bhw_id\b)\s*,", body)
        dsp_match = re.search(r"dsp_version:\s*(\d+),", body)
        tier_match = re.search(r"tier:\s*CapabilityTier::(\w+),", body)
        if not all([name_match, hw_match, dsp_match, tier_match]):
            continue
        templates[fn_name] = RustProfileTemplate(
            fn_name=fn_name,
            name=name_match.group(1),
            hw_expr=(hw_match.group(1) or "hw_id").strip(),
            dsp_version=int(dsp_match.group(1)),
            tier=tier_match.group(1),
        )

    identify_body = re.search(
        r"pub fn identify\(hw_id: u8, dsp_version: u8\) -> DeviceProfile \{\s*match \(hw_id, dsp_version\) \{(?P<body>.*?)\n\s*\}\n\}",
        text,
        re.S,
    )
    if identify_body is None:
        raise ValueError("failed to parse identify() body from probe.rs")

    arm_pattern = re.compile(
        r"\((\d+),\s*(\d+|_)\)\s*=>\s*(profile_[a-z0-9_]+)\(([^)]*)\),"
    )
    profiles: dict[tuple[int, int], RustProfile] = {}
    for hw_match, dsp_match, fn_name, arg_expr in arm_pattern.findall(identify_body.group("body")):
        template = templates.get(fn_name)
        if template is None:
            continue
        hw_id = int(hw_match)
        if dsp_match == "_":
            dsp_version = template.dsp_version
        else:
            dsp_version = int(dsp_match)
        arg_expr = arg_expr.strip()
        if template.hw_expr == "hw_id" and arg_expr:
            resolved_hw = int(arg_expr)
        elif template.hw_expr.isdigit():
            resolved_hw = int(template.hw_expr)
        else:
            resolved_hw = hw_id
        profiles[(resolved_hw, dsp_version)] = RustProfile(
            name=template.name,
            hw_id=resolved_hw,
            dsp_version=dsp_version,
            tier=template.tier,
        )
    return sorted(profiles.values(), key=lambda p: (p.hw_id, p.dsp_version, p.name))


def build_matrix() -> dict[str, Any]:
    modern_devices, usb_entries = parse_top_level_modern_devices()
    legacy_configs = parse_legacy_configs()
    android_hints = parse_android_hints()
    desktop_fallback_hints = parse_desktop_fallback_hints()
    package_inventory = parse_package_inventory()
    device_type_assets = parse_device_type_assets()
    dirac_binary_hints = parse_dirac_binary_hints()
    rust_profiles = parse_rust_profiles()

    enriched_usb_entries = []
    for entry in usb_entries:
        resolution = USB_HINT_RESOLUTION.get(
            entry["label"],
            {
                "status": "unknown",
                "resolution": "no explicit generated classification yet",
            },
        )
        enriched = {**entry, **resolution}
        if entry["label"] == "Harmony DSP" and dirac_binary_hints["contains_harmony_dsp_8x12"]:
            enriched.update(
                {
                    "status": "family_candidate",
                    "candidate_runtime_profiles": "Flex HT, Flex HTx",
                    "candidate_source_basis": (
                        "official Dirac helper strings: Harmony DSP 8x12 + assume dsp 113 + dsp id 115"
                    ),
                    "resolution": (
                        "official packaged assets still do not expose an exact hw_id/dsp_id row, "
                        "but the Dirac helper binary explicitly references `Harmony DSP 8x12` and "
                        "DSP IDs `113`/`115`, which strongly suggests the same 8x12 HT-family DSP "
                        "structures as Flex HT / Flex HTx"
                    ),
                }
            )
        if entry["label"] == "CDSP-8x12/DL" and dirac_binary_hints["contains_padsp_8x12_dl"]:
            enriched["candidate_source_basis"] = (
                "official USB PID label + source-backed cached legacy C-DSP 8x12 v2 family + "
                "Dirac helper string PADSP-8x12-DL"
            )
            enriched["resolution"] = (
                "name strongly suggests the same 8x12 custom-install family as the cached "
                "C-DSP 8x12 v2 profile, and the packaged Dirac helper also contains "
                "`PADSP-8x12-DL`, but the extracted assets still do not expose an exact "
                "hw_id/dsp_id mapping"
            )
        enriched_usb_entries.append(enriched)

    rust_by_pair = {(p.hw_id, p.dsp_version): p for p in rust_profiles}
    rust_name_pairs = {(p.hw_id, p.dsp_version, p.name) for p in rust_profiles}
    modern_rows = []
    for device in sorted(modern_devices, key=lambda d: (d["hw_id"], d["dsp_version"])):
        pair = (device["hw_id"], device["dsp_version"])
        rust = rust_by_pair.get(pair)
        modern_rows.append(
            {
                **device,
                "rust_profile": asdict(rust) if rust else None,
                "status": "implemented" if rust else "missing",
            }
        )

    implemented_pairs = {pair for pair in rust_by_pair}
    modern_pairs = {(d["hw_id"], d["dsp_version"]) for d in modern_devices}
    asset_pairs = {
        (entry["hw_id"], entry["dsp_version"]) for entry in device_type_assets["product_assets"]
    }
    extra_rust_profiles = [
        asdict(p) for p in rust_profiles if (p.hw_id, p.dsp_version) not in modern_pairs
    ]
    desktop_fallback_models = [
        asdict(
            DesktopFallbackModel(
                dsp_id=dsp_id,
                models=sorted(
                    {device["product_name"] for device in modern_devices if device["dsp_version"] == dsp_id}
                ),
            )
        )
        for dsp_id in sorted({device["dsp_version"] for device in modern_devices})
    ]
    minidsp_rs_reference_rows = []
    for row in MINIDSP_RS_REFERENCE_PROFILES:
        profile = rust_by_pair.get((row["hw_id"], row["dsp_version"]))
        minidsp_rs_reference_rows.append(
            {
                **row,
                "covered_by_rust": profile is not None,
                "rust_profile": profile.name if profile else None,
                "name_match": (row["hw_id"], row["dsp_version"], row["name"]) in rust_name_pairs,
            }
        )

    return {
        "modern_devices": modern_rows,
        "legacy_configs": legacy_configs,
        "usb_product_hints": enriched_usb_entries,
        "android_hints": android_hints,
        "desktop_fallback_hints": desktop_fallback_hints,
        "desktop_fallback_models": desktop_fallback_models,
        "package_inventory": package_inventory,
        "device_type_assets": device_type_assets,
        "dirac_binary_hints": dirac_binary_hints,
        "minidsp_rs_reference_profiles": minidsp_rs_reference_rows,
        "rust_profiles": [asdict(p) for p in rust_profiles],
        "extra_rust_profiles": extra_rust_profiles,
        "summary": {
            "modern_exact_count": len(modern_rows),
            "modern_implemented_count": sum(1 for row in modern_rows if row["status"] == "implemented"),
            "modern_missing_count": sum(1 for row in modern_rows if row["status"] == "missing"),
            "legacy_config_count": len(legacy_configs),
            "rust_profile_count": len(rust_profiles),
            "android_source_group_count": len(android_hints["source_groups"]),
            "usb_product_hint_count": len(usb_entries),
            "usb_product_hint_unresolved_exact_count": sum(
                1 for entry in enriched_usb_entries if entry.get("exact_runtime_known") != "true"
            ),
            "usb_product_hint_alias_only_count": sum(
                1 for entry in enriched_usb_entries if entry["status"] == "alias_only"
            ),
            "usb_product_hint_family_candidate_count": sum(
                1 for entry in enriched_usb_entries if entry["status"] == "family_candidate"
            ),
            "desktop_fallback_model_count": len(desktop_fallback_models),
            "package_exact_hw_count": len(package_inventory["exact_hw_ids"]),
            "package_exact_dsp_count": len(package_inventory["exact_dsp_ids"]),
            "device_type_product_asset_count": len(device_type_assets["product_assets"]),
            "device_type_product_asset_exact_match_count": sum(
                1 for pair in asset_pairs if pair in modern_pairs
            ),
            "minidsp_rs_reference_count": len(minidsp_rs_reference_rows),
            "minidsp_rs_reference_covered_count": sum(
                1 for row in minidsp_rs_reference_rows if row["covered_by_rust"]
            ),
            "desktop_fallback_catalog_present": desktop_fallback_hints["main_handler_present"]
            and desktop_fallback_hints["ui_service_present"],
        },
    }


def format_sources(sources: list[dict[str, Any]]) -> str:
    parts = []
    for entry in sources:
        index = entry.get("value", entry.get("idx"))
        parts.append(f"{index}:{entry['name']}")
    return ", ".join(parts)


def render_markdown(matrix: dict[str, Any]) -> str:
    lines = [
        "# Official App Device Coverage",
        "",
        "Generated by `tools/extract_device_support_matrix.py` from:",
        "- `app/MiniDSP Device Console.app/Contents/Resources/device_support_package/`",
        "- `app/apk-extracted/assets/www/js/controllers.js`",
        "- `protocol/src/device/probe.rs`",
        "- generated Rust mirror: `protocol/src/device/generated_official.rs`",
        "",
        "This file is the repeatable coverage snapshot for comparing official-app device data against the current Rust support table.",
        "",
        "## Summary",
        "",
        f'- Exact modern HW/DSP pairs in Device Console package: `{matrix["summary"]["modern_exact_count"]}`',
        f'- Exact modern HW/DSP pairs implemented in `probe.rs`: `{matrix["summary"]["modern_implemented_count"]}`',
        f'- Exact modern HW/DSP pairs still missing in `probe.rs`: `{matrix["summary"]["modern_missing_count"]}`',
        f'- Cached legacy `config.xml` sources available locally: `{matrix["summary"]["legacy_config_count"]}`',
        f'- Total Rust profiles in `probe.rs`: `{matrix["summary"]["rust_profile_count"]}`',
        f'- Android source-map groups parsed from APK logic: `{matrix["summary"]["android_source_group_count"]}`',
        f'- USB product hints in top-level Device Console package: `{matrix["summary"]["usb_product_hint_count"]}`',
        f'- USB product hints still lacking exact runtime identity: `{matrix["summary"]["usb_product_hint_unresolved_exact_count"]}`',
        f'- USB product hints with only alias labels and no family mapping: `{matrix["summary"]["usb_product_hint_alias_only_count"]}`',
        f'- USB product hints with a source-backed family candidate but no exact identity: `{matrix["summary"]["usb_product_hint_family_candidate_count"]}`',
        f'- Desktop fallback-model catalog path present: `{matrix["summary"]["desktop_fallback_catalog_present"]}`',
        f'- Desktop fallback-model groups extracted from packaged app logic: `{matrix["summary"]["desktop_fallback_model_count"]}`',
        f'- Exact HW directories present in the packaged Device Console assets: `{matrix["summary"]["package_exact_hw_count"]}`',
        f'- Exact DSP directories present in the packaged Device Console assets: `{matrix["summary"]["package_exact_dsp_count"]}`',
        f'- Product image rows in the extracted desktop assets: `{matrix["summary"]["device_type_product_asset_count"]}`',
        f'- Product image rows matching exact modern HW/DSP pairs: `{matrix["summary"]["device_type_product_asset_exact_match_count"]}`',
        f'- `minidsp-rs` reference profiles checked: `{matrix["summary"]["minidsp_rs_reference_count"]}`',
        f'- `minidsp-rs` reference profiles covered by `probe.rs`: `{matrix["summary"]["minidsp_rs_reference_covered_count"]}`',
        "",
        "## Exact Modern Package Boundary",
        "",
        "These lists are the strongest repeatable evidence that the current packaged desktop assets only carry the exact modern matrix already enumerated above. The unresolved USB aliases do not have their own HW directory, DSP directory, or product image row in the extracted package.",
        "",
        f'- Exact HW directories: `{matrix["package_inventory"]["exact_hw_ids"]}`',
        f'- Exact DSP directories: `{matrix["package_inventory"]["exact_dsp_ids"]}`',
        f'- Extra desktop device-type assets: `{matrix["device_type_assets"]["extra_assets"]}`',
        "",
        "| Product Asset | HW | DSP |",
        "|---------------|----|-----|",
    ]

    for asset in matrix["device_type_assets"]["product_assets"]:
        lines.append(
            f'| `{asset["filename"]}` | `{asset["hw_id"]}` | `{asset["dsp_version"]}` |'
        )

    lines.extend(
        [
            "",
        "## Exact Modern HW/DSP Pairs",
        "",
        "| HW | DSP | Official Product | Rust Status | Rust Tier | Sources | Flags |",
        "|----|-----|------------------|-------------|-----------|---------|-------|",
        ]
    )

    for row in matrix["modern_devices"]:
        hw = row["hw_info"]
        rust = row["rust_profile"]
        flags = []
        if hw["rew_supported"]:
            flags.append("REW")
        if hw["has_display"]:
            flags.append("Display")
        if hw["has_bluetooth_module"]:
            flags.append("BT")
        if hw["has_dre_option"]:
            flags.append("DRE")
        if hw["audio_src_has_voloffsets"]:
            flags.append("VolOffsets")
        lines.append(
            f'| `{row["hw_id"]}` | `{row["dsp_version"]}` | {row["product_name"]} | '
            f'{row["status"]} | {rust["tier"] if rust else "missing"} | '
            f'{format_sources(hw["audio_sources"])} | {", ".join(flags) if flags else "-"} |'
        )

    lines.extend(
        [
            "",
            "## DSP Config Typing Snapshot",
            "",
            "The Device Console XML exposes typed config storage directly through `rep`, `range`, and default hex values. This is the clean-room source for systematizing config typing beyond the 2x4 HD workaround.",
            "",
            "| DSP | Rep Counts | Filter Counts | Example Typed Slots | Ranges |",
            "|-----|------------|---------------|---------------------|--------|",
        ]
    )
    seen_dsp_ids = set()
    for row in matrix["modern_devices"]:
        dsp_defaults = row["dsp_defaults"]
        dsp_id = row["dsp_version"]
        if dsp_id in seen_dsp_ids:
            continue
        seen_dsp_ids.add(dsp_id)
        rep_counts = ", ".join(f"{rep}:{count}" for rep, count in sorted(dsp_defaults["rep_counts"].items()))
        filter_counts = ", ".join(
            f"{filter_type}:{count}" for filter_type, count in sorted(dsp_defaults["filter_counts"].items())
        )
        example_parts = []
        for rep, examples in sorted(dsp_defaults["rep_examples"].items()):
            if not examples:
                continue
            ex = examples[0]
            example_parts.append(f'{rep}={ex["name"]}@{ex["idx"]}')
        lines.append(
            f'| `{dsp_id}` | {rep_counts} | {filter_counts} | {"; ".join(example_parts)} | {", ".join(dsp_defaults["range_names"])} |'
        )

    lines.extend(
        [
            "",
            "## Cached Legacy Config Sources",
            "",
            "These SigmaStudio `config.xml` files are now cached locally under `third_party/legacy-configs/` so legacy-family support can be driven by repeatable source data instead of doc-only references.",
            "",
            "| Device | HW | DSP | Dialect | Labels | Items | Filters | Max Addr | Path |",
            "|--------|----|-----|---------|--------|-------|---------|----------|------|",
        ]
    )
    for row in matrix["legacy_configs"]:
        dsp_value = "-" if row["dsp_version"] is None else str(row["dsp_version"])
        lines.append(
            f'| {row["name"]} | `{row["hw_id"]}` | `{dsp_value}` | {row["dialect"]} | '
            f'`{row["label_count"]}` | `{row["item_count"]}` | `{row["filter_count"]}` | '
            f'`{row["max_addr"]}` | `{row["path"]}` |'
        )

    lines.extend(
        [
            "",
            "## Legacy Filter Typing Snapshot",
            "",
            "| Device | Declared DSP | Filter Types |",
            "|--------|--------------|--------------|",
        ]
    )
    for row in matrix["legacy_configs"]:
        filter_types = ", ".join(
            f"{name}:{count}" for name, count in sorted(row["filter_type_counts"].items())
        )
        lines.append(
            f'| {row["name"]} | `{row["declared_dsp_version"]}` | {filter_types or "-"} |'
        )

    lines.extend(
        [
            "",
            "## Legacy Runtime Metadata Snapshot",
            "",
            "These counts come directly from the cached `config.xml` corpus and are intended to drive importer/codegen work instead of hand-maintained profile guesses.",
            "",
            "| Device | In Labels | In Unique | Out Labels | Out Unique | Meter In | Meter Out | Meter Comp | FIR | Compressor | PEQ Ch | Max PEQ Bands | Inferred In | Inferred Out |",
            "|--------|-----------|-----------|------------|------------|----------|-----------|------------|-----|------------|--------|---------------|-------------|--------------|",
        ]
    )
    for row in matrix["legacy_configs"]:
        lines.append(
            f'| {row["name"]} | `{row["input_label_count"]}` | `{row["unique_input_label_text_count"]}` | `{row["output_label_count"]}` | `{row["unique_output_label_text_count"]}` | '
            f'`{row["meter_input_count"]}` | `{row["meter_output_count"]}` | `{row["meter_compressor_count"]}` | '
            f'`{row["fir_count"]}` | `{row["compressor_count"]}` | `{row["peq_channel_count"]}` | '
            f'`{row["max_peq_bands_per_channel"]}` | `{row["inferred_input_channels"]}` | `{row["inferred_output_channels"]}` |'
        )

    lines.extend(
        [
            "",
            "## Android Source-Map Groups",
            "",
            "These are not exact product definitions by themselves, but they are useful official-app evidence for source maps and legacy-family coverage.",
            "",
            "| HW Group | Variant | Sources |",
            "|----------|---------|---------|",
        ]
    )
    for group in matrix["android_hints"]["source_groups"]:
        variant = group.get("variant", "-")
        lines.append(
            f'| `{",".join(str(v) for v in group["hw_ids"])}` | {variant} | {format_sources(group["sources"])} |'
        )

    lines.extend(
        [
            "",
            f'Android Dirac DSP versions: `{matrix["android_hints"]["dirac_dsp_versions"]}`',
            "",
            f'Android select-index offset hw_ids: `{matrix["android_hints"]["select_index_offset_hw_ids"]}`',
            "",
            "## Desktop Fallback-Model Catalog",
            "",
            "The extracted desktop app exposes a user-facing blank-device fallback-model picker. The decoded main-process logic groups `deviceSupportPkg.deviceIdList` by `dspId`, so this is evidence about the same exact modern device matrix already captured above rather than a hidden alias-identity catalog.",
            "",
            "| Signal | Present |",
            "|--------|---------|",
            f'| Main-process IPC handler `{matrix["desktop_fallback_hints"]["handler_name"]}` | `{matrix["desktop_fallback_hints"]["main_handler_present"]}` |',
            f'| UI service method `getDspIdFallbackModels()` | `{matrix["desktop_fallback_hints"]["ui_service_present"]}` |',
            f'| UI setting key `fallback-dspId-value` | `{matrix["desktop_fallback_hints"]["ui_setting_key_present"]}` |',
            f'| UI prompt `Select a miniDSP Model` | `{matrix["desktop_fallback_hints"]["ui_prompt_present"]}` |',
            f'| Main export `allSupportedDevices => deviceSupportPkg.deviceIdList` | `{matrix["desktop_fallback_hints"]["all_supported_devices_exports_device_id_list"]}` |',
            f'| Exact model list extracted into this report | `{matrix["desktop_fallback_hints"]["exact_model_list_extracted"]}` |',
            "",
            matrix["desktop_fallback_hints"]["notes"],
            "",
            "| DSP | Desktop Fallback Models |",
            "|-----|-------------------------|",
        ]
    )
    for entry in matrix["desktop_fallback_models"]:
        lines.append(f'| `{entry["dsp_id"]}` | {", ".join(entry["models"])} |')

    lines.extend(
        [
            "",
            "## Packaged Dirac Binary Alias Hints",
            "",
            "These strings come from `Contents/Resources/dirac/minidsp_dirac`. They do not provide an exact `hw_id`, but they are stronger official evidence than the top-level USB PID comments for a subset of unresolved aliases.",
            "",
            "| Signal | Present |",
            "|--------|---------|",
            f'| `Harmony DSP 8x12` | `{matrix["dirac_binary_hints"]["contains_harmony_dsp_8x12"]}` |',
            f'| `PADSP-8x12-DL` | `{matrix["dirac_binary_hints"]["contains_padsp_8x12_dl"]}` |',
            f'| `Failed to get dsp id, assume it is 113` | `{matrix["dirac_binary_hints"]["contains_assume_dsp_113"]}` |',
            f'| `dsp id is 115` | `{matrix["dirac_binary_hints"]["contains_dsp_id_115"]}` |',
            "",
            matrix["dirac_binary_hints"]["notes"],
            "",
            "## USB Product Hints Present In Official Package",
            "",
            "These come from the top-level Device Console package USB list. Some appear in the USB PID list even when they do not have a full HW/DSP definition in the packaged XML set.",
            "",
            "| VID | PID | Official Label | Status | Exact Runtime Known | Candidate Profiles | Resolution |",
            "|-----|-----|----------------|--------|---------------------|--------------------|------------|",
        ]
    )
    for entry in matrix["usb_product_hints"]:
        lines.append(
            f'| `{entry["vid"]}` | `{entry["pid"]}` | {entry["label"]} | {entry["status"]} | '
            f'`{entry.get("exact_runtime_known", "false")}` | {entry.get("candidate_runtime_profiles", "-")} | {entry["resolution"]} |'
        )

    lines.extend(
        [
            "",
            "## minidsp-rs Reference Cross-Check",
            "",
            "This is a secondary consistency check against the upstream `minidsp-rs` device set. When official app assets and `minidsp-rs` disagree, the official extracted assets remain the source of truth.",
            "",
            "| HW | DSP | minidsp-rs Name | Covered By Rust | Rust Profile | Name Match |",
            "|----|-----|------------------|-----------------|-------------|------------|",
        ]
    )
    for row in matrix["minidsp_rs_reference_profiles"]:
        lines.append(
            f'| `{row["hw_id"]}` | `{row["dsp_version"]}` | {row["name"]} | '
            f'`{row["covered_by_rust"]}` | {row["rust_profile"] or "-"} | `{row["name_match"]}` |'
        )

    lines.extend(
        [
            "",
            "## Rust Profiles Not Represented As Exact Modern XML Pairs",
            "",
            "These are mostly legacy/basic-support definitions and app-derived additions that do not appear as exact Device Console HW/DSP rows.",
            "",
            "| HW | DSP | Rust Name | Tier |",
            "|----|-----|-----------|------|",
        ]
    )
    for row in matrix["extra_rust_profiles"]:
        lines.append(f'| `{row["hw_id"]}` | `{row["dsp_version"]}` | {row["name"]} | {row["tier"]} |')

    lines.extend(
        [
            "",
            "## Immediate Gaps To Close",
            "",
            "- Keep the importer/report pipeline generated from official assets instead of hand-maintained notes.",
            "- Treat the remaining unresolved USB-only labels conservatively: `HA-DSP` and `DDRC-22` are still alias-only, while `DDRC-88`, `CDSP-8x12/DL`, and `Harmony DSP` are family candidates without exact identities.",
            "- Do not treat the desktop fallback-model picker as a hidden alias-identity catalog; it is sourced from `deviceSupportPkg.deviceIdList` grouped by DSP ID.",
            "- Keep checking against `minidsp-rs`, but treat it as a secondary reference rather than the contract source when it diverges from the official assets.",
            "- Keep this report regenerated as the source of truth before claiming new device coverage in permanent docs.",
            "",
        ]
    )
    return "\n".join(lines)


def rust_bool(value: bool) -> str:
    return "true" if value else "false"


def render_rust(matrix: dict[str, Any]) -> str:
    lines = [
        "// Generated by tools/extract_device_support_matrix.py. Do not edit by hand.",
        "",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub struct OfficialModernDevice {",
        "    pub hw_id: u8,",
        "    pub dsp_version: u8,",
        "    pub product_name: &'static str,",
        "    pub source_count: usize,",
        "    pub rew_supported: bool,",
        "    pub has_display: bool,",
        "    pub has_bluetooth: bool,",
        "    pub has_dre: bool,",
        "    pub has_volume_offsets: bool,",
        "}",
        "",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub struct OfficialUsbProductHint {",
        "    pub vid: u16,",
        "    pub pid: u16,",
        "    pub label: &'static str,",
        "    pub status: &'static str,",
        "    pub exact_runtime_known: bool,",
        "    pub candidate_runtime_profiles: &'static [&'static str],",
        "}",
        "",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub struct OfficialDesktopFallbackModel {",
        "    pub dsp_id: u8,",
        "    pub models: &'static [&'static str],",
        "}",
        "",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub struct OfficialLegacyDevice {",
        "    pub hw_id: u8,",
        "    pub dsp_version: Option<u8>,",
        "    pub product_name: &'static str,",
        "    pub dialect: &'static str,",
        "    pub input_label_count: usize,",
        "    pub unique_input_label_text_count: usize,",
        "    pub output_label_count: usize,",
        "    pub unique_output_label_text_count: usize,",
        "    pub meter_input_count: usize,",
        "    pub meter_output_count: usize,",
        "    pub meter_compressor_count: usize,",
        "    pub fir_count: usize,",
        "    pub compressor_count: usize,",
        "    pub peq_channel_count: usize,",
        "    pub max_peq_bands_per_channel: usize,",
        "    pub inferred_input_channels: usize,",
        "    pub inferred_output_channels: usize,",
        "}",
        "",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub struct MinidspRsReferenceProfile {",
        "    pub hw_id: u8,",
        "    pub dsp_version: u8,",
        "    pub name: &'static str,",
        "    pub covered_by_rust: bool,",
        "    pub rust_profile: Option<&'static str>,",
        "    pub name_match: bool,",
        "}",
        "",
        "pub static OFFICIAL_MODERN_DEVICES: &[OfficialModernDevice] = &[",
    ]
    for row in matrix["modern_devices"]:
        hw = row["hw_info"]
        lines.extend(
            [
                "    OfficialModernDevice {",
                f'        hw_id: {row["hw_id"]},',
                f'        dsp_version: {row["dsp_version"]},',
                f'        product_name: {json.dumps(row["product_name"])},',
                f'        source_count: {len(hw["audio_sources"])},',
                f'        rew_supported: {rust_bool(hw["rew_supported"])},',
                f'        has_display: {rust_bool(hw["has_display"])},',
                f'        has_bluetooth: {rust_bool(hw["has_bluetooth_module"])},',
                f'        has_dre: {rust_bool(hw["has_dre_option"])},',
                f'        has_volume_offsets: {rust_bool(hw["audio_src_has_voloffsets"])},',
                "    },",
            ]
        )
    lines.extend(["];", "", "pub static OFFICIAL_USB_PRODUCT_HINTS: &[OfficialUsbProductHint] = &["])
    for entry in matrix["usb_product_hints"]:
        lines.extend(
            [
                "    OfficialUsbProductHint {",
                f'        vid: {int(entry["vid"], 16)},',
                f'        pid: {int(entry["pid"], 16)},',
                f'        label: {json.dumps(entry["label"])},',
                f'        status: {json.dumps(entry["status"])},',
                f'        exact_runtime_known: {rust_bool(entry.get("exact_runtime_known") == "true")},',
                "        candidate_runtime_profiles: &["
                + ", ".join(
                    json.dumps(candidate.strip())
                    for candidate in entry.get("candidate_runtime_profiles", "").split(",")
                    if candidate.strip()
                )
                + "],",
                "    },",
            ]
        )
    lines.extend(
        [
            "];",
            "",
            "pub static OFFICIAL_DESKTOP_FALLBACK_MODELS: &[OfficialDesktopFallbackModel] = &[",
        ]
    )
    for entry in matrix["desktop_fallback_models"]:
        model_list = ", ".join(json.dumps(model) for model in entry["models"])
        lines.extend(
            [
                "    OfficialDesktopFallbackModel {",
                f'        dsp_id: {entry["dsp_id"]},',
                f"        models: &[{model_list}],",
                "    },",
            ]
        )
    lines.extend(
        [
            "];",
            "",
            "pub static OFFICIAL_LEGACY_DEVICES: &[OfficialLegacyDevice] = &[",
        ]
    )
    for row in matrix["legacy_configs"]:
        dsp_version = f'Some({row["dsp_version"]})' if row["dsp_version"] is not None else "None"
        lines.extend(
            [
                "    OfficialLegacyDevice {",
                f'        hw_id: {row["hw_id"]},',
                f"        dsp_version: {dsp_version},",
                f'        product_name: {json.dumps(row["name"])},',
                f'        dialect: {json.dumps(row["dialect"])},',
                f'        input_label_count: {row["input_label_count"]},',
                f'        unique_input_label_text_count: {row["unique_input_label_text_count"]},',
                f'        output_label_count: {row["output_label_count"]},',
                f'        unique_output_label_text_count: {row["unique_output_label_text_count"]},',
                f'        meter_input_count: {row["meter_input_count"]},',
                f'        meter_output_count: {row["meter_output_count"]},',
                f'        meter_compressor_count: {row["meter_compressor_count"]},',
                f'        fir_count: {row["fir_count"]},',
                f'        compressor_count: {row["compressor_count"]},',
                f'        peq_channel_count: {row["peq_channel_count"]},',
                f'        max_peq_bands_per_channel: {row["max_peq_bands_per_channel"]},',
                f'        inferred_input_channels: {row["inferred_input_channels"]},',
                f'        inferred_output_channels: {row["inferred_output_channels"]},',
                "    },",
            ]
        )
    lines.extend(
        [
            "];",
            "",
            "pub static OFFICIAL_DIRAC_DSP_VERSIONS: &[u8] = &[",
        ]
    )
    for dsp_version in matrix["android_hints"]["dirac_dsp_versions"]:
        lines.append(f"    {dsp_version},")
    lines.extend(["];", "", "pub static MINIDSP_RS_REFERENCE_PROFILES: &[MinidspRsReferenceProfile] = &["])
    for row in matrix["minidsp_rs_reference_profiles"]:
        rust_profile = (
            json.dumps(row["rust_profile"]) if row["rust_profile"] is not None else "None"
        )
        if rust_profile != "None":
            rust_profile = f"Some({rust_profile})"
        lines.extend(
            [
                "    MinidspRsReferenceProfile {",
                f'        hw_id: {row["hw_id"]},',
                f'        dsp_version: {row["dsp_version"]},',
                f'        name: {json.dumps(row["name"])},',
                f'        covered_by_rust: {rust_bool(row["covered_by_rust"])},',
                f"        rust_profile: {rust_profile},",
                f'        name_match: {rust_bool(row["name_match"])},',
                "    },",
            ]
        )
    lines.extend(["];", ""])
    return "\n".join(lines)


def main() -> None:
    matrix = build_matrix()
    OUTPUT_JSON.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT_JSON.write_text(json.dumps(matrix, indent=2, sort_keys=True) + "\n")
    OUTPUT_MD.write_text(render_markdown(matrix))
    OUTPUT_RS.write_text(render_rust(matrix))
    print(f"Wrote {OUTPUT_MD.relative_to(REPO_ROOT)}")
    print(f"Wrote {OUTPUT_JSON.relative_to(REPO_ROOT)}")
    print(f"Wrote {OUTPUT_RS.relative_to(REPO_ROOT)}")


if __name__ == "__main__":
    main()
