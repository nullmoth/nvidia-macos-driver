#![no_main]
#![no_std]

// NullMoth boot logo. macOS boot.efi decodes its embedded Apple logo itself and draws it with one GOP Blt
// (EfiBltBufferToVideo, 84x103, or 168x206 on HiDPI, centred on the screen; measured on a 1920x1080 boot). This driver
// wraps Blt and draws the NullMoth logo, blended onto the boot background, centred where the Apple logo was. Every
// other draw passes through unchanged. Nothing on disk is modified. Boot argument -nmlogooff turns it off; Setup can
// also disable this driver in the OpenCore config.

extern crate alloc;
use alloc::vec::Vec;
use core::ffi::c_void;
use uefi::prelude::*;
use uefi::runtime::{self, VariableAttributes, VariableVendor};
use uefi::{cstr16, guid};

const LOG_VENDOR: VariableVendor = VariableVendor(guid!("4e4d4f54-484c-4f47-8000-6e6d6c6f676f"));
const APPLE_BOOT: VariableVendor = VariableVendor(guid!("7c436110-ab2a-4bbb-a880-fe41995c9f82"));

static LOGO_1X: &[u8] = include_bytes!("../assets/logo1x.rgba");
static LOGO_2X: &[u8] = include_bytes!("../assets/logo2x.rgba");
const LOGO_1X_W: usize = 144; const LOGO_1X_H: usize = 180;
const LOGO_2X_W: usize = 288; const LOGO_2X_H: usize = 360;

type BltFn = unsafe extern "efiapi" fn(*mut c_void, *mut u32, u32, usize, usize, usize, usize, usize, usize, usize) -> Status;
static mut ORIG_BLT: Option<BltFn> = None;
static mut BG: u32 = 0;          // background pixel (BGRA as the firmware stores it), from the full-screen fill
static mut DONE: bool = false;

fn note(msg: &[u8]) {
    let attrs = VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS | VariableAttributes::RUNTIME_ACCESS;
    let _ = runtime::set_variable(cstr16!("nm-logo"), &LOG_VENDOR, attrs, msg);
}

fn disabled_by_boot_arg() -> bool {
    let mut buf = [0u8; 1024];
    match runtime::get_variable(cstr16!("boot-args"), &APPLE_BOOT, &mut buf) {
        Ok((v, _)) => v.split(|&b| b == b' ' || b == 0).any(|w| w == b"-nmlogooff"),
        Err(_) => false,
    }
}

// the logo blended over `bg`, as GOP pixels (B, G, R, reserved)
fn render(rgba: &[u8], w: usize, h: usize, bg: u32) -> Vec<u32> {
    let (bb, bgc, br) = (bg & 0xFF, (bg >> 8) & 0xFF, (bg >> 16) & 0xFF);
    let mut out = Vec::with_capacity(w * h);
    for i in 0..w * h {
        let (r, g, b, a) = (rgba[i * 4] as u32, rgba[i * 4 + 1] as u32, rgba[i * 4 + 2] as u32, rgba[i * 4 + 3] as u32);
        let mix = |c: u32, base: u32| (c * a + base * (255 - a)) / 255;
        out.push(mix(b, bb) | (mix(g, bgc) << 8) | (mix(r, br) << 16));
    }
    out
}

unsafe extern "efiapi" fn blt(this: *mut c_void, buf: *mut u32, op: u32, sx: usize, sy: usize, dx: usize, dy: usize,
                              w: usize, h: usize, delta: usize) -> Status {
    unsafe {
        let orig = (*core::ptr::addr_of!(ORIG_BLT)).unwrap();
        // EfiBltVideoFill (0) of the whole screen: the background the logo sits on
        if op == 0 && !buf.is_null() && w >= 640 && h >= 480 { *core::ptr::addr_of_mut!(BG) = *buf; }
        // EfiBltBufferToVideo (2) of the Apple logo, at 1x or 2x
        let big = (w, h) == (168, 206);
        if op == 2 && ((w, h) == (84, 103) || big) {
            let (src, lw, lh) = if big { (LOGO_2X, LOGO_2X_W, LOGO_2X_H) } else { (LOGO_1X, LOGO_1X_W, LOGO_1X_H) };
            let (cx, cy) = (dx + w / 2, dy + h / 2);
            if cx >= lw / 2 && cy >= lh / 2 {
                let mut px = render(src, lw, lh, *core::ptr::addr_of!(BG));
                let r = orig(this, px.as_mut_ptr(), 2, 0, 0, cx - lw / 2, cy - lh / 2, lw, lh, 0);
                if !*core::ptr::addr_of!(DONE) {
                    *core::ptr::addr_of_mut!(DONE) = true;
                    note(if r.is_success() { b"replaced" } else { b"draw-failed" });
                }
                if r.is_success() { return r; }
            }
        }
        orig(this, buf, op, sx, sy, dx, dy, w, h, delta)
    }
}

#[entry]
fn main() -> Status {
    if disabled_by_boot_arg() { note(b"off (-nmlogooff)"); return Status::SUCCESS; }
    let Ok(h) = boot::get_handle_for_protocol::<uefi::proto::console::gop::GraphicsOutput>() else { note(b"no-gop"); return Status::SUCCESS; };
    let Ok(mut g) = (unsafe { boot::open_protocol::<uefi::proto::console::gop::GraphicsOutput>(
        boot::OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None },
        boot::OpenProtocolAttributes::GetProtocol) }) else { note(b"gop-open-failed"); return Status::SUCCESS; };
    // EFI_GRAPHICS_OUTPUT_PROTOCOL = { QueryMode, SetMode, Blt, Mode* }: Blt is the third pointer
    unsafe {
        let slot = ((&mut *g) as *mut _ as *mut usize).add(2);
        *core::ptr::addr_of_mut!(ORIG_BLT) = Some(core::mem::transmute::<usize, BltFn>(*slot));
        *slot = blt as usize;
    }
    note(b"armed");
    Status::SUCCESS
}
