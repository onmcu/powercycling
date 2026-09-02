//! Troubleshooting output.

use std::io::Write;

use crate::device::{Device, DeviceId};
use crate::pairing::HubPairs;
use crate::power::PowerPorts;
use crate::sysfs::{device_location, read_serial};
use crate::tree::tree;

/// Write what each stage of the search sees to `out`.
///
/// A troubleshooting aid for when [`PowerPorts::find`] fails or a device
/// refuses to disappear. Every USB failure, including a failing
/// [`PowerPorts::find`], is part of the report rather than returned.
///
/// # Errors
///
/// Only if `out` could not be written to.
pub fn debug_scan(
    device: &DeviceId,
    pairs: &HubPairs,
    out: &mut impl Write,
) -> std::io::Result<()> {
    writeln!(
        out,
        "-- devices matching {:04x}:{:04x}",
        device.vid, device.pid
    )?;
    let devices = match rusb::devices() {
        Ok(devices) => devices,
        Err(e) => return writeln!(out, "   bus could not be enumerated: {e}"),
    };
    let candidates: Vec<Device> = devices
        .iter()
        .filter(|d| {
            d.device_descriptor()
                .is_ok_and(|desc| desc.vendor_id() == device.vid && desc.product_id() == device.pid)
        })
        .collect();
    if candidates.is_empty() {
        writeln!(out, "   none - not enumerated at all")?;
    }
    for dev in &candidates {
        let loc = device_location(dev);
        let found = read_serial(dev);
        let verdict = if device.matches(dev) {
            "<= MATCH"
        } else if found.is_empty() {
            "(serial unreadable - permissions?)"
        } else {
            "(serial differs)"
        };
        writeln!(out, "   {loc}  serial={found:?}  {verdict}")?;
    }

    writeln!(out, "-- usb tree")?;
    tree(pairs, out)?;

    match PowerPorts::find(device, pairs) {
        Err(e) => writeln!(out, "-- search failed: {e}"),
        Ok(ports) => writeln!(out, "-- {ports}"),
    }
}
