//! The USB tree, annotated with what matters for power cycling.

use std::collections::HashMap;
use std::io::Write;

use rusb::Version;

use crate::device::Device;
use crate::hub::{Hub, Hubs, is_hub};
use crate::pairing::{HubPairs, Pairing};
use crate::sysfs::{child_location, device_location, read_serial, read_sysfs};

/// One enumerated device.
struct Entry {
    location: String,
    bus: u8,
    path: Vec<u8>,
    vid: u16,
    pid: u16,
    version: Version,
    product: String,
    serial: String,
    /// `(bNbrPorts, per-port power switching)` for a hub that could be
    /// opened, `None` for a device or an unreadable hub.
    hub: Option<(u8, bool)>,
    is_hub: bool,
}

impl Entry {
    fn read(dev: &Device) -> Option<Self> {
        let desc = dev.device_descriptor().ok()?;
        let location = device_location(dev);
        let is_hub = is_hub(dev);
        Some(Self {
            bus: dev.bus_number(),
            path: dev.port_numbers().ok()?,
            vid: desc.vendor_id(),
            pid: desc.product_id(),
            version: desc.usb_version(),
            product: read_sysfs(&location, "product"),
            serial: read_serial(dev),
            hub: is_hub
                .then(|| Hub::open(dev.clone()).ok())
                .flatten()
                .map(|hub| (hub.nports, hub.per_port_power)),
            is_hub,
            location,
        })
    }
}

/// Write the USB tree to `out`, annotated for power cycling.
///
/// Every bus from its root hub down; each hub with its version, identity,
/// port count, switching mode and other half; each port with the device on
/// it, or `-` when empty. Unpaired hubs are flagged, and what to do about them
/// follows the tree. `pairs` is what the machine declares; it decides the
/// "other half" column.
///
/// # Errors
///
/// Only if `out` could not be written to.
pub fn tree(pairs: &HubPairs, out: &mut impl Write) -> std::io::Result<()> {
    let (devices, hubs) = match (rusb::devices(), Hubs::enumerate()) {
        (Ok(devices), Ok(hubs)) => (devices, hubs),
        (Err(e), _) | (_, Err(e)) => return writeln!(out, "bus could not be enumerated: {e}"),
    };
    let pairing = Pairing::compute(&hubs, pairs);
    let entries: Vec<Entry> = devices.iter().filter_map(|d| Entry::read(&d)).collect();
    let by_location: HashMap<&str, &Entry> =
        entries.iter().map(|e| (e.location.as_str(), e)).collect();

    let mut roots: Vec<&Entry> = entries.iter().filter(|e| e.path.is_empty()).collect();
    roots.sort_by_key(|e| e.bus);
    for root in roots {
        write_hub(root, "", &by_location, &pairing, out)?;
    }

    for hub in pairing.declared_absent() {
        writeln!(out, "note: {hub} is declared as a pair but not on the bus")?;
    }
    let unpaired = pairing.unpaired();
    if !unpaired.is_empty() {
        writeln!(
            out,
            "\n{} one half of a USB 3.x hub whose other half could not be identified. \
             Both halves keep a receptacle\npowered, so a device on such a hub cannot \
             be power-cycled until the pair is known. The bus cannot tell;\nprobe the \
             hub with a device that has a power LED, and declare the pair it finds \
             in your hub pairs.",
            match unpaired.as_slice() {
                [one] => format!("{one} is"),
                many => format!("{} are each", many.join(", ")),
            }
        )?;
    }
    Ok(())
}

/// One hub and, indented below it, each of its ports.
fn write_hub(
    hub: &Entry,
    prefix: &str,
    by_location: &HashMap<&str, &Entry>,
    pairing: &Pairing,
    out: &mut impl Write,
) -> std::io::Result<()> {
    let switching = hub.hub.map_or(
        "unreadable",
        |(_, ppps)| {
            if ppps { "ppps" } else { "ganged" }
        },
    );
    let nports = hub
        .hub
        .map_or_else(String::new, |(n, _)| format!("{n} ports  "));
    writeln!(
        out,
        "{:<12} {}  USB {}.{}{}  {nports}{switching:<6}  {}",
        hub.location,
        identity(hub),
        hub.version.major(),
        hub.version.minor(),
        hub.version.sub_minor(),
        pairing.verdict(&hub.location)
    )?;

    // Ports from the descriptor; for an unreadable hub, whatever is enumerated
    // below it.
    let ports: Vec<u8> = hub.hub.map_or_else(
        || {
            let mut found: Vec<u8> = by_location
                .values()
                .filter(|e| e.bus == hub.bus && e.path.len() == hub.path.len() + 1)
                .filter(|e| e.path[..hub.path.len()] == hub.path[..])
                .filter_map(|e| e.path.last().copied())
                .collect();
            found.sort_unstable();
            found
        },
        |(n, _)| (1..=n).collect(),
    );
    let last = ports.last().copied();
    for port in ports {
        let (branch, below) = if Some(port) == last {
            ("└─ ", "   ")
        } else {
            ("├─ ", "│  ")
        };
        write!(out, "{prefix}{branch}port {port}: ")?;
        match by_location.get(child_location(&hub.location, hub.bus, port).as_str()) {
            None => writeln!(out, "-")?,
            Some(child) if child.is_hub => {
                write_hub(
                    child,
                    &format!("{prefix}{below}"),
                    by_location,
                    pairing,
                    out,
                )?;
            }
            Some(child) => writeln!(out, "{:<12} {}", child.location, identity(child))?,
        }
    }
    Ok(())
}

/// `vid:pid`, the product string and the serial, as far as they are known.
fn identity(entry: &Entry) -> String {
    let product = if entry.product.is_empty() {
        String::new()
    } else {
        format!("  {:?}", entry.product)
    };
    let serial = if entry.serial.is_empty() {
        String::new()
    } else {
        format!("  serial {}", entry.serial)
    };
    format!("{:04x}:{:04x}{product}{serial}", entry.vid, entry.pid)
}
