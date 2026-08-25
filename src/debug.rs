//! Troubleshooting output.

use crate::device::{Device, matches_id};
use crate::error::Result;
use crate::hub::{Hub, all_hubs};
use crate::port::HubPort;
use crate::power::PowerPorts;
use crate::sysfs::{read_serial, sysfs_location};

/// Print what each stage of the search sees.
///
/// Writes to stdout: a troubleshooting aid for when [`PowerPorts::find`] fails
/// or a device refuses to disappear, not something to call from a service.
///
/// # Errors
///
/// [`crate::Error::Usb`] if the bus could not be enumerated. A failing
/// [`PowerPorts::find`] is printed, not returned.
pub fn debug_scan(vid: u16, pid: u16, serial: Option<&str>) -> Result<()> {
    println!("-- devices matching {vid:04x}:{pid:04x}");
    let candidates: Vec<Device> = rusb::devices()?
        .iter()
        .filter(|d| {
            d.device_descriptor()
                .is_ok_and(|desc| desc.vendor_id() == vid && desc.product_id() == pid)
        })
        .collect();
    if candidates.is_empty() {
        println!("   none - not enumerated at all");
    }
    for dev in &candidates {
        let loc = sysfs_location(dev.bus_number(), &dev.port_numbers().unwrap_or_default());
        let found = read_serial(dev);
        let verdict = if matches_id(dev, vid, pid, serial) {
            "<= MATCH"
        } else if found.is_empty() {
            "(serial unreadable - permissions?)"
        } else {
            "(serial differs)"
        };
        println!("   {loc}  serial={found:?}  {verdict}");
    }

    println!("-- hubs");
    for (dev, loc) in all_hubs()? {
        match Hub::open(dev, &loc) {
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

    match PowerPorts::find(vid, pid, serial) {
        Err(e) => println!("-- search failed: {e}"),
        Ok(ports) => {
            println!("-- cutting {:?}", ports.primary);
            match ports.held() {
                [] => println!("   nothing held down (USB 2.0 only receptacle)"),
                held => {
                    let names: Vec<String> = held.iter().map(HubPort::location).collect();
                    println!("   holding down {}: {}", held.len(), names.join(", "));
                }
            }
        }
    }
    Ok(())
}
