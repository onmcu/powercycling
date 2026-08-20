//! Hubs, and whether they can switch power per port.

use rusb::constants::{
    LIBUSB_CLASS_HUB, LIBUSB_DT_HUB, LIBUSB_DT_SUPERSPEED_HUB, LIBUSB_REQUEST_GET_DESCRIPTOR,
};
use rusb::{DeviceHandle, Direction, GlobalContext, Recipient, RequestType, Version, request_type};
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::sysfs::{SYSFS_USB, sysfs_location};
use crate::{Device, TIMEOUT};

/// Logical Power Switching Mode mask and its per-port value, from
/// `wHubCharacteristics` (USB 2.0 §11.23.2.1).
const HUB_CHAR_LPSM: u8 = 0x03;
const HUB_CHAR_INDV_PORT_LPSM: u8 = 0x01;

/// A hub declaring at least this may be the USB 2.0 half of a USB 3.x
/// receptacle. One declaring USB 2.0 has no SuperSpeed half.
const USB_BOS: Version = Version(2, 1, 0);
/// A hub declaring at least this is a SuperSpeed hub.
pub(crate) const USB_SS: Version = Version(3, 0, 0);

/// One logical hub. A USB 3.x receptacle is fed by two of these.
#[derive(Clone)]
pub(crate) struct Hub {
    pub(crate) dev: Device,
    /// sysfs location, e.g. `2-1.2`, or `usb2` for a root hub.
    pub(crate) location: String,
    pub(crate) bus: u8,
    pub(crate) path: Vec<u8>,
    pub(crate) version: Version,
    pub(crate) nports: u8,
    pub(crate) per_port_power: bool,
}

impl Hub {
    /// Open a hub and read its descriptor.
    ///
    /// Requires usbfs access: whether a hub switches power per port is only in
    /// the hub descriptor, not in sysfs.
    pub(crate) fn open(dev: Device, location: &str) -> Result<Hub> {
        let unreadable = || Error::HubUnreadable {
            location: location.to_string(),
        };
        let desc = dev.device_descriptor().map_err(|_| unreadable())?;
        // The declared spec version, not the negotiated link speed: a
        // SuperSpeed hub plugged into a USB 2.0 port is still the SS half.
        let version = desc.usb_version();
        let handle = dev.open().map_err(|_| unreadable())?;
        let (nports, per_port_power) =
            read_hub_descriptor(&handle, version >= USB_SS).map_err(|_| unreadable())?;

        Ok(Hub {
            bus: dev.bus_number(),
            path: dev.port_numbers().unwrap_or_default(),
            location: location.to_string(),
            dev,
            version,
            nports,
            per_port_power,
        })
    }

    pub(crate) fn is_super_speed(&self) -> bool {
        self.version >= USB_SS
    }

    /// Whether this hub can be one half of a USB 3.x receptacle.
    pub(crate) fn may_have_peer(&self) -> bool {
        self.version >= USB_BOS
    }

    /// sysfs directory of one downstream port, e.g.
    /// `/sys/bus/usb/devices/2-1:1.0/2-1-port3`.
    ///
    /// The config number comes from the cached descriptor, so this needs no
    /// usbfs handle.
    pub(crate) fn port_dir(&self, port: u8) -> rusb::Result<PathBuf> {
        let cfg = self.dev.active_config_descriptor()?.number();
        // A root hub's interface directory is `<bus>-0:<cfg>.0`, not `usb<bus>:...`.
        let iface = if self.path.is_empty() {
            format!("{}-0", self.bus)
        } else {
            self.location.clone()
        };
        Ok(PathBuf::from(format!(
            "{SYSFS_USB}/{iface}:{cfg}.0/{}-port{port}",
            self.location
        )))
    }
}

/// Read `bNbrPorts` and whether the hub switches power per port.
fn read_hub_descriptor(
    handle: &DeviceHandle<GlobalContext>,
    super_speed: bool,
) -> rusb::Result<(u8, bool)> {
    let rt = request_type(Direction::In, RequestType::Class, Recipient::Device);
    let dt = if super_speed {
        LIBUSB_DT_SUPERSPEED_HUB
    } else {
        LIBUSB_DT_HUB
    };

    let mut buf = [0u8; 12];
    let n = handle.read_control(
        rt,
        LIBUSB_REQUEST_GET_DESCRIPTOR,
        u16::from(dt) << 8,
        0,
        &mut buf,
        TIMEOUT,
    )?;
    if n < 9 {
        return Err(rusb::Error::Io);
    }

    let nports = buf[2];
    let lpsm = buf[3] & HUB_CHAR_LPSM;
    // With one port, ganged switching and per-port switching are the same act.
    let per_port_power = lpsm == HUB_CHAR_INDV_PORT_LPSM || (lpsm == 0 && nports == 1);
    Ok((nports, per_port_power))
}

/// Every enumerated hub with its sysfs location. Reads cached descriptors only,
/// opens nothing.
pub(crate) fn all_hubs() -> rusb::Result<Vec<(Device, String)>> {
    Ok(rusb::devices()?
        .iter()
        .filter(|dev| {
            dev.device_descriptor()
                .is_ok_and(|d| d.class_code() == LIBUSB_CLASS_HUB)
        })
        .map(|dev| {
            let loc = sysfs_location(dev.bus_number(), &dev.port_numbers().unwrap_or_default());
            (dev, loc)
        })
        .collect())
}

/// Open the hub at `location`.
pub(crate) fn open_hub_at(hubs: &[(Device, String)], location: &str) -> Result<Hub> {
    let (dev, _) =
        hubs.iter()
            .find(|(_, l)| l == location)
            .ok_or_else(|| Error::HubUnreadable {
                location: location.to_string(),
            })?;
    Hub::open(dev.clone(), location)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_thresholds() {
        // A USB 2.0 hub has no SuperSpeed half; 2.1 and up may have one.
        assert!(Version(2, 0, 0) < USB_BOS);
        assert!(Version(2, 1, 0) >= USB_BOS);
        assert!(Version(2, 1, 0) < USB_SS);
        // Declared spec version, so 3.2 still counts as SuperSpeed.
        assert!(Version(3, 0, 0) >= USB_SS);
        assert!(Version(3, 2, 0) >= USB_SS);
    }
}
