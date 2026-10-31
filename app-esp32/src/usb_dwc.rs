//! Read-only snapshot of the USB OTG controller (Synopsys DWC2) registers.
//!
//! Logged when a HID transfer never completes and again while the host
//! library's channel halt is still waiting. That halt waits for a channel
//! interrupt with no timeout, so these registers are the only way to tell a
//! channel that is still being scheduled from one that stopped, or from one
//! that halted with an interrupt nobody serviced.

use log::warn;
use serde_json::{json, Value};
use std::time::Duration;

/// `USB_DWC` in the ESP32-S3 peripheral map.
const DWC_BASE: usize = 0x6008_0000;
const NUM_CHANNELS: usize = 8;
/// The host library's periodic frame list length (`FRAME_LIST_LEN` in hcd_dwc.c).
const FRAME_LIST_LEN: usize = 32;

const GINTSTS: usize = 0x014;
const GINTMSK: usize = 0x018;
const GNPTXSTS: usize = 0x02C;
const HCFG: usize = 0x400;
const HFNUM: usize = 0x408;
const HPTXSTS: usize = 0x410;
const HAINT: usize = 0x414;
const HAINTMSK: usize = 0x418;
const HFLBADDR: usize = 0x41C;
const HPRT: usize = 0x440;
const HC_BASE: usize = 0x500;
const HC_STRIDE: usize = 0x20;
const HCCHAR: usize = 0x00;
const HCINT: usize = 0x08;
const HCINTMSK: usize = 0x0C;
const HCTSIZ: usize = 0x10;
const HCDMA: usize = 0x14;
const HCDMAB: usize = 0x1C;
const HCCHAR_CHENA: u32 = 1 << 31;

fn reg(offset: usize) -> u32 {
    // SAFETY: fixed MMIO addresses of the OTG controller, which the host
    // library has powered and clocked. None of these registers changes state
    // on read (the interrupt registers clear on write, not read).
    unsafe { core::ptr::read_volatile((DWC_BASE + offset) as *const u32) }
}

/// The controller only fetches descriptors and the frame list from internal
/// DRAM or PSRAM; anything else in these registers is not safe to follow.
fn readable_ram(addr: u32, len: usize) -> bool {
    let end = addr as usize + len;
    let internal = (0x3FC8_8000..=0x3FD0_0000).contains(&(addr as usize)) && end <= 0x3FD0_0000;
    let psram = (0x3C00_0000..=0x3E00_0000).contains(&(addr as usize)) && end <= 0x3E00_0000;
    addr.is_multiple_of(4) && (internal || psram)
}

fn read_word(addr: u32) -> u32 {
    // SAFETY: callers check the address with `readable_ram` first.
    unsafe { core::ptr::read_volatile(addr as usize as *const u32) }
}

pub fn log_controller_state(context: &str) {
    let hfnum_before = reg(HFNUM);
    std::thread::sleep(Duration::from_millis(3));
    let hfnum_after = reg(HFNUM);
    warn!(
        "USB DWC {context}: gintsts={:08x} gintmsk={:08x} hprt={:08x} hcfg={:08x} hfnum={hfnum_before:08x}->{hfnum_after:08x}",
        reg(GINTSTS),
        reg(GINTMSK),
        reg(HPRT),
        reg(HCFG),
    );
    warn!(
        "USB DWC {context}: hptxsts={:08x} gnptxsts={:08x} haint={:08x} haintmsk={:08x}",
        reg(HPTXSTS),
        reg(GNPTXSTS),
        reg(HAINT),
        reg(HAINTMSK),
    );

    let frame_list = reg(HFLBADDR);
    let frame_list_ok = readable_ram(frame_list, FRAME_LIST_LEN * 4);
    for ch in 0..NUM_CHANNELS {
        let base = HC_BASE + ch * HC_STRIDE;
        let hcchar = reg(base + HCCHAR);
        if hcchar == 0 {
            continue;
        }
        let hcdma = reg(base + HCDMA);
        // Frame-list slots that schedule this channel (periodic endpoints only).
        let mut slots = 0u32;
        if frame_list_ok {
            for i in 0..FRAME_LIST_LEN {
                if read_word(frame_list + 4 * i as u32) & (1 << ch) != 0 {
                    slots |= 1 << i;
                }
            }
        }
        // The descriptor the channel is on: list base in [31:9], index in [8:3].
        let qtd_addr = (hcdma & 0xFFFF_FE00) + ((hcdma >> 3) & 0x3F) * 8;
        let (qtd_status, qtd_buffer) = if hcchar & HCCHAR_CHENA != 0 && readable_ram(qtd_addr, 8) {
            (read_word(qtd_addr), read_word(qtd_addr + 4))
        } else {
            (0, 0)
        };
        warn!(
            "USB DWC {context} ch{ch}: hcchar={hcchar:08x} hcint={:08x} hcintmsk={:08x} hctsiz={:08x} hcdma={hcdma:08x} hcdmab={:08x} slots={slots:08x} qtd={qtd_status:08x}/{qtd_buffer:08x}",
            reg(base + HCINT),
            reg(base + HCINTMSK),
            reg(base + HCTSIZ),
            reg(base + HCDMAB),
        );
    }
}

/// The same registers as JSON, for `GET /debug/usb`.
pub fn controller_state_json() -> Value {
    let hptxsts = reg(HPTXSTS);
    let channels: Vec<Value> = (0..NUM_CHANNELS)
        .map(|ch| HC_BASE + ch * HC_STRIDE)
        .filter(|&base| reg(base + HCCHAR) != 0)
        .map(|base| {
            json!({
                "hcchar": format!("{:08x}", reg(base + HCCHAR)),
                "hcint": format!("{:08x}", reg(base + HCINT)),
                "hctsiz": format!("{:08x}", reg(base + HCTSIZ)),
                "hcdma": format!("{:08x}", reg(base + HCDMA)),
            })
        })
        .collect();
    json!({
        "gintsts": format!("{:08x}", reg(GINTSTS)),
        "hprt": format!("{:08x}", reg(HPRT)),
        "hfnum": format!("{:08x}", reg(HFNUM)),
        "hptxsts": format!("{hptxsts:08x}"),
        "periodicTxFifoFreeWords": hptxsts & 0xFFFF,
        "periodicTxQueueFree": (hptxsts >> 16) & 0xFF,
        "gnptxsts": format!("{:08x}", reg(GNPTXSTS)),
        "channels": channels,
    })
}
