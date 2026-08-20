//! sysfs facts about devices: locations, port directories, serials.

use crate::device::Device;

pub(crate) const SYSFS_USB: &str = "/sys/bus/usb/devices";

/// sysfs location of a device, e.g. `2-1.2.1.1`, or `usb2` for a root hub.
pub(crate) fn sysfs_location(bus: u8, path: &[u8]) -> String {
    if path.is_empty() {
        format!("usb{bus}")
    } else {
        let p: Vec<String> = path.iter().map(u8::to_string).collect();
        format!("{bus}-{}", p.join("."))
    }
}

/// Split a port directory name, e.g. `2-1.2-port3` or `usb2-port1`, into the
/// owning hub's location and the port number.
pub(crate) fn split_port_dir(name: &str) -> Option<(&str, u8)> {
    let (loc, port) = name.rsplit_once("-port")?;
    Some((loc, port.parse().ok()?))
}

/// Read a device's serial from sysfs, or empty if it publishes none.
///
/// Not a string-descriptor read: that needs usbfs write permission, which would
/// make discovery require the same privileges as switching.
pub(crate) fn read_serial(dev: &Device) -> String {
    let loc = sysfs_location(dev.bus_number(), &dev.port_numbers().unwrap_or_default());
    std::fs::read_to_string(format!("{SYSFS_USB}/{loc}/serial"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_locations() {
        assert_eq!(sysfs_location(2, &[]), "usb2");
        assert_eq!(sysfs_location(2, &[1]), "2-1");
        assert_eq!(sysfs_location(2, &[1, 2, 3]), "2-1.2.3");
    }

    #[test]
    fn port_directory_names() {
        assert_eq!(split_port_dir("usb2-port1"), Some(("usb2", 1)));
        assert_eq!(split_port_dir("2-1.2-port3"), Some(("2-1.2", 3)));
        assert_eq!(split_port_dir("2-1.2"), None);
        assert_eq!(split_port_dir("usb2-portX"), None);
    }
}
