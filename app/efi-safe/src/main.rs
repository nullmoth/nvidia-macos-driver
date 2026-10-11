#![no_main]
#![no_std]

extern crate alloc;
use alloc::vec::Vec;
use core::time::Duration;
use uefi::prelude::*;
use uefi::runtime::{self, VariableAttributes, VariableVendor};
use uefi::proto::console::text::Key;
use uefi::{cstr16, guid, print, println, Char16};

const APPLE_BOOT: VariableVendor = VariableVendor(guid!("7c436110-ab2a-4bbb-a880-fe41995c9f82"));
const SAFE: &[u8] = b"-nvoff";

fn has_safe(args: &[u8]) -> bool {
    args.split(|&b| b == b' ' || b == 0).any(|w| w == SAFE)
}

fn fail(msg: &str, st: Status) -> Status {
    println!("NullMoth: {} ({:?}). Nothing was changed.", msg, st);
    println!("Nothing was changed. Choose macOS to start normally.");
    boot::stall(Duration::from_secs(8));
    Status::ABORTED
}

// Waits up to 60 s for one key. Only Y goes ahead: this entry sits in the boot picker next to macOS, and one stray
// Enter used to remove the driver with no question asked.
// The picker entry's Arguments: "once" = start macOS one time without the driver (Discord 10-11: "an option to temporarily
// disable the driver so I can boot recovery"). Only -nvoff is written: OpenCore rewrites boot-args at the next start
// (NVRAM Delete), so the driver is back by itself after this one start. No remove flag, nothing uninstalled.
fn once_mode() -> bool {
    let Ok(li) = boot::open_protocol_exclusive::<uefi::proto::loaded_image::LoadedImage>(boot::image_handle()) else { return false };
    let Some(opts) = li.load_options_as_bytes() else { return false };
    let text: Vec<u8> = opts.chunks(2).filter_map(|c| c.first().copied()).filter(|&b| b != 0).collect();
    text.split(|&b| b == b' ').any(|w| w.eq_ignore_ascii_case(b"once"))
}

fn confirmed(once: bool) -> bool {
    // The Enter that chose this entry in the picker can still be queued: read as "not Y", it closed the tool at once and
    // the screen went straight back to booting ("restarts immediately", Discord 10-11). Drop anything already typed.
    boot::stall(Duration::from_millis(300));
    for _ in 0..64 {
        match uefi::system::with_stdin(|stdin| stdin.read_key()) {
            Ok(Some(_)) => continue,
            _ => break,
        }
    }
    println!("");
    if once {
        println!("  NullMoth: start macOS once without the NVIDIA driver?");
        println!("  Press Y, then choose your macOS disk (or Recovery). The driver is back at the start after that.");
        println!("  Any other key, or waiting 60 seconds, changes nothing.");
    } else {
        println!("  NullMoth: remove the NVIDIA driver?");
        println!("  Press Y to remove it at the next macOS start. Any other key, or waiting 60 seconds, changes nothing.");
    }
    print!("  > ");
    let yes = [Char16::try_from('y').unwrap(), Char16::try_from('Y').unwrap()];
    for _ in 0..1200 {
        let key = uefi::system::with_stdin(|stdin| stdin.read_key());
        match key {
            Ok(Some(Key::Printable(c))) => return yes.contains(&c),
            Ok(Some(Key::Special(_))) => return false,
            _ => boot::stall(Duration::from_millis(50)),
        }
    }
    false
}

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();
    let _ = uefi::system::with_stdin(|stdin| stdin.reset(false));
    let once = once_mode();
    if !confirmed(once) {
        println!("");
        println!("  Nothing was changed. Choose macOS to start normally.");
        boot::stall(Duration::from_secs(4));
        return Status::ABORTED;
    }
    let name = cstr16!("boot-args");
    let mut buf = [0u8; 1024];
    let current: Vec<u8> = match runtime::get_variable(name, &APPLE_BOOT, &mut buf) {
        Ok((v, _)) => v.iter().copied().take_while(|&b| b != 0).collect(),
        Err(e) if e.status() == Status::NOT_FOUND => Vec::new(),
        Err(e) => return fail("could not read boot-args", e.status()),
    };
    if has_safe(&current) {
        println!("NullMoth: the next macOS start already has the NVIDIA driver off.");
    } else {
        let mut next = current.clone();
        if !next.is_empty() {
            next.push(b' ');
        }
        next.extend_from_slice(SAFE);
        let attrs = VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS | VariableAttributes::RUNTIME_ACCESS;
        if let Err(e) = runtime::set_variable(name, &APPLE_BOOT, attrs, &next) {
            return fail("could not write boot-args", e.status());
        }
        let mut chk = [0u8; 1024];
        if !matches!(runtime::get_variable(name, &APPLE_BOOT, &mut chk), Ok((v, _)) if has_safe(v)) {
            return fail("the firmware did not keep the change", Status::DEVICE_ERROR);
        }
    }
    if once {
        println!("");
        println!("  NullMoth: the NVIDIA driver is off for this start only.");
        println!("  Choose your macOS disk or Recovery now. Restarting brings the driver back.");
        boot::stall(Duration::from_secs(5));
        return Status::SUCCESS;
    }
    let attrs = VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS | VariableAttributes::RUNTIME_ACCESS;
    if let Err(e) = runtime::set_variable(cstr16!("nullmoth-remove"), &APPLE_BOOT, attrs, b"1") {
        return fail("could not set the remove flag", e.status());
    }
    println!("");
    println!("  NullMoth: the next macOS start removes the NVIDIA driver and restarts by itself.");
    println!("  Choose your macOS disk now, or restart: the driver stays off until it has been removed.");
    println!("  The screen stays on while it works.");
    println!("  Afterwards this Mac is back to how it was before NullMoth was installed.");
    boot::stall(Duration::from_secs(6));
    Status::SUCCESS
}
