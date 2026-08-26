//! Troubleshooting output.

use crate::device::{Device, DeviceId};
use crate::error::Result;
use crate::hub::{Hub, Hubs};
use crate::port::HubPort;
use crate::power::PowerPorts;
use crate::sysfs::{device_location, read_serial};

/// Print what each stage of the search sees.
///
/// Writes to stdout: a troubleshooting aid for when [`PowerPorts::find`] fails
/// or a device refuses to disappear, not something to call from a service.
///
/// # Errors
///
/// [`crate::Error::Usb`] if the bus could not be enumerated. A failing
/// [`PowerPorts::find`] is printed, not returned.
pub fn debug_scan(device: &DeviceId) -> Result<()> {
    println!("-- devices matching {:04x}:{:04x}", device.vid, device.pid);
    let candidates: Vec<Device> = rusb::devices()?
        .iter()
        .filter(|d| {
            d.device_descriptor()
                .is_ok_and(|desc| desc.vendor_id() == device.vid && desc.product_id() == device.pid)
        })
        .collect();
    if candidates.is_empty() {
        println!("   none - not enumerated at all");
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
        println!("   {loc}  serial={found:?}  {verdict}");
    }

    println!("-- hubs");
    for dev in Hubs::enumerate()?.iter() {
        let loc = device_location(dev);
        match Hub::open(dev.clone()) {
            Ok(hub) => println!(
                "   {:<12} USB {}.{}  {} ports  {}",
                hub.location,
                hub.version.major(),
                hub.version.minor(),
                hub.nports,
                if hub.per_port_power { "ppps" } else { "ganged" },
            ),
            Err(e) => println!("   {loc:<12} {e}"),
        }
    }

    match PowerPorts::find(device) {
        Err(e) => println!("-- search failed: {e}"),
        Ok(ports) => {
            println!("-- cutting {:?}", ports.primary);
            match ports.held() {
                [] => println!("   nothing held down (no opposite-speed port to hold)"),
                held => {
                    let names: Vec<String> = held.iter().map(HubPort::label).collect();
                    println!("   holding down {}: {}", held.len(), names.join(", "));
                }
            }
        }
    }
    Ok(())
}
