//! Hubs, and whether they can switch power per port.

use rusb::constants::{
    LIBUSB_CLASS_HUB, LIBUSB_DT_HUB, LIBUSB_DT_SUPERSPEED_HUB, LIBUSB_REQUEST_GET_DESCRIPTOR,
};
use rusb::{DeviceHandle, Direction, GlobalContext, Recipient, RequestType, Version, request_type};
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::sysfs::{SYSFS_USB, device_location};
use crate::{Device, TIMEOUT};

/// Logical Power Switching Mode mask and its per-port value, from
/// `wHubCharacteristics` (USB 2.0 §11.23.2.1).
const HUB_CHAR_LPSM: u8 = 0x03;
const HUB_CHAR_INDV_PORT_LPSM: u8 = 0x01;

/// Shortest hub descriptor that can be read: the USB 2.0 one for up to seven
/// ports (§11.23.2.1). The `SuperSpeed` one is a fixed 12 bytes (USB 3.2
/// §10.15.2.1).
const HUB_DESC_MIN_LEN: usize = 9;

/// A hub declaring at least this may be the USB 2.0 half of a USB 3.x
/// receptacle. One declaring USB 2.0 has no `SuperSpeed` half.
pub const USB_BOS: Version = Version(2, 1, 0);
/// A hub declaring at least this is a `SuperSpeed` hub.
pub const USB_SS: Version = Version(3, 0, 0);

/// One logical hub. A USB 3.x receptacle is fed by two of these.
#[derive(Clone)]
pub struct Hub {
    /// Device on the global libusb context
    pub dev: Device,
    /// sysfs location, e.g. `2-1.2`, or `usb2` for a root hub.
    pub location: String,
    /// Bus number this hub is connected to
    pub bus: u8,
    /// USB version
    pub version: Version,
    /// `bNbrPorts`, the number of ports this hub has
    pub nports: u8,
    /// Whether the hub supports PPPS
    pub per_port_power: bool,
}

impl Hub {
    /// Open a hub and read its descriptor.
    ///
    /// Requires usbfs access: whether a hub switches power per port is only in
    /// the hub descriptor, not in sysfs.
    ///
    /// # Errors
    ///
    /// [`Error::HubUnreadable`] if the hub could not be opened or would not
    /// answer.
    pub fn open(dev: Device) -> Result<Self> {
        let location = device_location(&dev);
        let unreadable = |source| Error::HubUnreadable {
            location: location.clone(),
            source,
        };

        let desc = dev.device_descriptor().map_err(unreadable)?;

        // The declared spec version, not the negotiated link speed: a
        // SuperSpeed hub plugged into a USB 2.0 port is still the SS half.
        let version = desc.usb_version();

        let handle = dev.open().map_err(unreadable)?;
        let (nports, per_port_power) =
            read_hub_descriptor(&handle, version >= USB_SS).map_err(unreadable)?;

        Ok(Self {
            bus: dev.bus_number(),
            location,
            version,
            dev,
            nports,
            per_port_power,
        })
    }

    /// Whether this is the bus's root hub, i.e. nothing above it in the tree.
    /// Its USB port path is empty, so sysfs names it `usb<bus>` instead of
    /// `<bus>-<port path>`, which every location built from it must account for.
    pub fn is_root_hub(&self) -> bool {
        self.dev.port_numbers().unwrap_or_default().is_empty()
    }
    /// Whether this is at least a USB 3 hub
    pub fn is_super_speed(&self) -> bool {
        self.version >= USB_SS
    }

    /// sysfs directory of one downstream port, e.g.
    /// `/sys/bus/usb/devices/2-1:1.0/2-1-port3`.
    ///
    /// The config number comes from the cached descriptor, so this needs no
    /// usbfs handle.
    pub fn port_dir(&self, port: u8) -> rusb::Result<PathBuf> {
        let cfg = self.dev.active_config_descriptor()?.number();
        // A root hub's interface directory is `<bus>-0:<cfg>.0`, not `usb<bus>:...`.
        let iface = if self.is_root_hub() {
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
    parse_hub_descriptor(&buf[..n]).ok_or(rusb::Error::Io)
}

/// `bNbrPorts` and whether the hub switches power per port, from the leading
/// bytes of a hub descriptor. Both descriptor types share the layout of these
/// fields (USB 2.0 §11.23.2.1, USB 3.2 §10.15.2.1).
///
/// `None` if the descriptor is too short to hold them.
const fn parse_hub_descriptor(desc: &[u8]) -> Option<(u8, bool)> {
    if desc.len() < HUB_DESC_MIN_LEN {
        return None;
    }

    // bNbrPorts is at offset 2, wHubCharacteristics at offset 3, with the
    // Logical Power Switching Mode in its two lowest bits:
    // 00 ganged, 01 per port, 1x reserved (no switching, USB 1.0 hubs).
    let nports = desc[2];
    let lpsm = desc[3] & HUB_CHAR_LPSM;

    // With one port, ganged switching and per-port switching are the same act.
    let per_port_power = lpsm == HUB_CHAR_INDV_PORT_LPSM || (lpsm == 0 && nports == 1);
    Some((nports, per_port_power))
}

/// Whether `dev` is a hub, by device class. An unreadable descriptor is not.
pub fn is_hub(dev: &Device) -> bool {
    dev.device_descriptor()
        .is_ok_and(|d| d.class_code() == LIBUSB_CLASS_HUB)
}

/// Every hub enumerated on the bus, unopened.
///
/// Only [`Hubs::enumerate`] builds one, and it filters on the hub device class,
/// so holding a `Hubs` is evidence that filter ran: nothing in it is an
/// ordinary device.
pub struct Hubs(Vec<Device>);

impl Hubs {
    /// Enumerate every hub. Reads cached descriptors only, opens nothing.
    ///
    /// # Errors
    ///
    /// Whatever enumerating the bus returns.
    pub fn enumerate() -> rusb::Result<Self> {
        Ok(Self(rusb::devices()?.iter().filter(is_hub).collect()))
    }

    /// Open the hub at `location`.
    ///
    /// `location` is a sysfs location, e.g. `2-1.2` or `usb2`, as produced by
    /// [`sysfs_location`](crate::sysfs::sysfs_location) or
    /// [`device_location`]
    ///
    /// # Errors
    ///
    /// [`Error::HubMissing`] if no hub sits at `location`, which means the
    /// topology moved under the search, or [`Error::HubUnreadable`] if the hub
    /// is there but will not open.
    pub fn open_at(&self, location: &str) -> Result<Hub> {
        let dev = self.device_at(location).ok_or_else(|| Error::HubMissing {
            location: location.to_string(),
        })?;
        Hub::open(dev.clone())
    }

    /// The hub at `location`, unopened, if one is enumerated there.
    pub fn device_at(&self, location: &str) -> Option<&Device> {
        self.0.iter().find(|dev| device_location(dev) == location)
    }

    /// The hubs, still unopened.
    pub fn iter(&self) -> impl Iterator<Item = &Device> {
        self.0.iter()
    }
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

    /// A USB 2.0 hub descriptor for `nports` ports with the given
    /// `wHubCharacteristics` low byte, as a hub with up to seven ports sends
    /// it: 7 fixed bytes plus one byte each of `DeviceRemovable` and
    /// `PortPwrCtrlMask`.
    fn usb2_desc(nports: u8, characteristics: u8) -> [u8; 9] {
        [
            9,
            LIBUSB_DT_HUB,
            nports,
            characteristics,
            0x00,
            50,
            0,
            0x00,
            0xff,
        ]
    }

    #[test]
    fn per_port_switching_is_lpsm_01() {
        assert_eq!(parse_hub_descriptor(&usb2_desc(4, 0b01)), Some((4, true)));
        // Other characteristics bits (overcurrent, TT think time, indicators)
        // do not matter.
        assert_eq!(parse_hub_descriptor(&usb2_desc(4, 0xfd)), Some((4, true)));
    }

    #[test]
    fn ganged_and_unswitched_hubs_are_not_per_port() {
        assert_eq!(parse_hub_descriptor(&usb2_desc(4, 0b00)), Some((4, false)));
        assert_eq!(parse_hub_descriptor(&usb2_desc(4, 0b10)), Some((4, false)));
        assert_eq!(parse_hub_descriptor(&usb2_desc(4, 0b11)), Some((4, false)));
    }

    #[test]
    fn single_port_ganged_hub_counts_as_per_port() {
        assert_eq!(parse_hub_descriptor(&usb2_desc(1, 0b00)), Some((1, true)));
        // Only ganged: a hub without power switching still cannot switch.
        assert_eq!(parse_hub_descriptor(&usb2_desc(1, 0b10)), Some((1, false)));
    }

    #[test]
    fn super_speed_descriptor_has_the_same_layout() {
        // bLength, bDescriptorType, bNbrPorts, wHubCharacteristics,
        // bPwrOn2PwrGood, bHubContrCurrent, bHubHdrDecLat, wHubDelay,
        // DeviceRemovable (USB 3.2 §10.15.2.1).
        let desc = [
            12,
            LIBUSB_DT_SUPERSPEED_HUB,
            4,
            0x09,
            0x00,
            50,
            0,
            0,
            0x00,
            0x00,
            0x00,
            0x00,
        ];
        assert_eq!(parse_hub_descriptor(&desc), Some((4, true)));
    }

    #[test]
    fn short_replies_are_rejected() {
        assert_eq!(parse_hub_descriptor(&[]), None);
        assert_eq!(parse_hub_descriptor(&usb2_desc(4, 0b01)[..8]), None);
    }
}
