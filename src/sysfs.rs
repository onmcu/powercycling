//! sysfs facts about devices: locations, port directories, serials.

use std::path::{Path, PathBuf};

use crate::device::Device;

pub const SYSFS_USB: &str = "/sys/bus/usb/devices";

/// sysfs location of a device, e.g. `2-1.2.1.1`, or `usb2` for a root hub.
pub fn sysfs_location(bus: u8, path: &[u8]) -> String {
    if path.is_empty() {
        format!("usb{bus}")
    } else {
        let p: Vec<String> = path.iter().map(u8::to_string).collect();
        format!("{bus}-{}", p.join("."))
    }
}

/// sysfs location of a device, from the device itself.
pub fn device_location(dev: &Device) -> String {
    sysfs_location(dev.bus_number(), &dev.port_numbers().unwrap_or_default())
}

/// sysfs location of the device on port `port` of the hub at `hub`, e.g.
/// `2-1.3` below `2-1`, or `3-2` below the root hub `usb3`.
pub fn child_location(hub: &str, bus: u8, port: u8) -> String {
    if hub.starts_with("usb") {
        format!("{bus}-{port}")
    } else {
        format!("{hub}.{port}")
    }
}

/// The host controller a root hub belongs to, as a sysfs path. The two root
/// hubs of one xHCI controller - its USB 2.0 and its `SuperSpeed` side -
/// share it.
pub fn controller_of(root_location: &str) -> Option<PathBuf> {
    std::fs::canonicalize(format!("{SYSFS_USB}/{root_location}"))
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

/// A device's `bDeviceClass` from sysfs, or `None` if it could not be read.
pub fn read_device_class(location: &str) -> Option<u8> {
    let s = std::fs::read_to_string(format!("{SYSFS_USB}/{location}/bDeviceClass")).ok()?;
    parse_device_class(&s)
}

/// `bDeviceClass` as sysfs prints it: two hex digits and a newline.
fn parse_device_class(s: &str) -> Option<u8> {
    u8::from_str_radix(s.trim(), 16).ok()
}

/// A device's sysfs attribute as trimmed text, or empty if unreadable.
pub fn read_sysfs(location: &str, attribute: &str) -> String {
    std::fs::read_to_string(format!("{SYSFS_USB}/{location}/{attribute}"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// A hub's `bNbrPorts` from sysfs (`maxchild`), or `None` if unreadable.
pub fn read_max_child(location: &str) -> Option<u8> {
    read_sysfs(location, "maxchild").parse().ok()
}

/// Read a device's serial from sysfs, or empty if it publishes none.
///
/// Not a string-descriptor read: that needs usbfs write permission, which would
/// make discovery require the same privileges as switching.
pub fn read_serial(dev: &Device) -> String {
    read_sysfs(&device_location(dev), "serial")
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
    fn device_class_is_hex() {
        assert_eq!(parse_device_class("09\n"), Some(0x09));
        assert_eq!(parse_device_class("ff"), Some(0xff));
        assert_eq!(parse_device_class(""), None);
        assert_eq!(parse_device_class("hub"), None);
    }

    #[test]
    fn child_locations() {
        assert_eq!(child_location("usb3", 3, 2), "3-2");
        assert_eq!(child_location("2-1", 2, 3), "2-1.3");
        assert_eq!(child_location("2-1.2", 2, 4), "2-1.2.4");
    }
}
