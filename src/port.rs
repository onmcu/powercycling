//! One downstream hub port, and the two ways to switch its power.

use rusb::constants::{LIBUSB_CLASS_HUB, LIBUSB_REQUEST_CLEAR_FEATURE, LIBUSB_REQUEST_SET_FEATURE};
use rusb::{DeviceHandle, Direction, GlobalContext, Recipient, RequestType, request_type};
use std::path::{Path, PathBuf};

use crate::TIMEOUT;
use crate::error::{Error, Result};
use crate::hub::Hub;
use crate::sysfs::{SYSFS_USB, child_location, read_device_class};

/// `PORT_POWER` feature selector (USB 2.0 §11.24.2, Table 11-17).
const PORT_POWER: u16 = 8;

/// One downstream port of one logical hub.
#[derive(Clone)]
pub struct HubPort {
    hub: Hub,
    port: u8,
    /// The port's sysfs directory, resolved once so switching needs no
    /// descriptor lookup.
    dir: PathBuf,
}

impl HubPort {
    pub(crate) fn new(hub: Hub, port: u8) -> Result<Self> {
        let dir = hub.port_dir(port)?;
        Ok(Self { hub, port, dir })
    }

    /// Whether this is the `SuperSpeed` half of a USB 3.x receptacle.
    #[must_use]
    pub fn is_super_speed(&self) -> bool {
        self.hub.is_super_speed()
    }

    /// sysfs location of the hub this port belongs to, e.g. `2-1.2.3`.
    #[must_use]
    pub fn hub_location(&self) -> &str {
        &self.hub.location
    }

    /// sysfs location a device plugged into this port would have, e.g.
    /// `2-1.2.3.4`. Everything at or below it loses power with the port.
    #[must_use]
    pub fn child_location(&self) -> String {
        child_location(&self.hub.location, self.hub.bus, self.port)
    }

    /// Whether this port can be held down alongside `primary` without
    /// stranding anything: it is empty, or both ports hold a hub - the two
    /// halves of one hub sit on one receptacle (USB 3.2 §10.1).
    pub(crate) fn is_holdable_for(&self, primary: &Self) -> bool {
        !self.is_occupied() || (primary.holds_hub() && self.holds_hub())
    }

    /// The hub this port belongs to.
    pub(crate) const fn hub(&self) -> &Hub {
        &self.hub
    }

    /// The port number on its hub.
    #[must_use]
    pub const fn port(&self) -> u8 {
        self.port
    }

    /// Whether something is plugged into this port.
    pub(crate) fn is_occupied(&self) -> bool {
        Path::new(SYSFS_USB).join(self.child_location()).exists()
    }

    /// Whether what is plugged into this port is a hub.
    ///
    /// Read from sysfs, so an empty port and an unreadable class both read
    /// `false`.
    pub(crate) fn holds_hub(&self) -> bool {
        read_device_class(&self.child_location()) == Some(LIBUSB_CLASS_HUB)
    }

    /// The port's `disable` attribute, if this kernel exposes one (6.0+).
    fn disable_attr(&self) -> Option<PathBuf> {
        let p = self.dir.join("disable");
        p.exists().then_some(p)
    }

    fn switch_failed(&self, usbfs: rusb::Error, sysfs: Option<std::io::Error>) -> Error {
        Error::SwitchFailed {
            port: self.to_string(),
            sysfs,
            usbfs,
        }
    }

    /// Switch this port, preferring the sysfs `disable` attribute.
    ///
    /// The kernel tracks `disable` and so keeps the port down; a raw usbfs
    /// request can be undone by port runtime PM. usbfs is the fallback, and
    /// both errors are reported if neither route works.
    ///
    /// # Errors
    ///
    /// [`Error::SwitchFailed`] if neither the sysfs `disable` attribute nor a
    /// usbfs control transfer switched the port.
    pub fn set_power(&self, on: bool) -> Result<()> {
        let mut sysfs_err = None;
        if let Some(path) = self.disable_attr() {
            match std::fs::write(path, if on { b"0" } else { b"1" }) {
                Ok(()) => return Ok(()),
                Err(e) => sysfs_err = Some(e),
            }
        }
        self.hub
            .dev
            .open()
            .and_then(|handle| write_port_power(&handle, self.port, on))
            .map_err(|usbfs| self.switch_failed(usbfs, sysfs_err))
    }
}

/// Send `SetPortFeature`/`ClearPortFeature(PORT_POWER)` for one port.
fn write_port_power(handle: &DeviceHandle<GlobalContext>, port: u8, on: bool) -> rusb::Result<()> {
    // bmRequestType 0x23: host->device, class, recipient = other (a port).
    let rt = request_type(Direction::Out, RequestType::Class, Recipient::Other);
    let req = if on {
        LIBUSB_REQUEST_SET_FEATURE
    } else {
        LIBUSB_REQUEST_CLEAR_FEATURE
    };
    handle.write_control(rt, req, PORT_POWER, u16::from(port), &[], TIMEOUT)?;
    Ok(())
}

/// Human-readable identifier, e.g. `2-1.2.3 port 4`.
///
/// For messages only. It is not a sysfs location and no path can be built
/// from it; [`Self::child_location`] is the sysfs one.
impl std::fmt::Display for HubPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} port {}", self.hub.location, self.port)
    }
}

impl std::fmt::Debug for HubPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{self} ({})",
            if self.is_super_speed() { "SS" } else { "HS" }
        )
    }
}
