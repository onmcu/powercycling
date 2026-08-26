//! Finding a device by `vid:pid:serial`, and waiting for it to come back.

use rusb::GlobalContext;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::sysfs::read_serial;

/// A device on the global libusb context.
///
/// The crate exposes no context parameter: there is one USB bus to talk to.
pub type Device = rusb::Device<GlobalContext>;

/// How often to re-check the bus while waiting for a device to appear.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The identity a device is looked up by.
///
/// The serial is owned because the identity outlives the lookup:
/// [`crate::PowerPorts`] re-checks the device once its VBUS is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceId {
    /// Vendor ID.
    pub vid: u16,
    /// Product ID.
    pub pid: u16,
    /// Serial number. `None` matches any serial.
    pub serial: Option<String>,
}

impl DeviceId {
    /// A device identified by `vid:pid`, optionally narrowed to one serial.
    #[must_use]
    pub fn new(vid: u16, pid: u16, serial: Option<&str>) -> Self {
        Self {
            vid,
            pid,
            serial: serial.map(String::from),
        }
    }

    /// Whether `dev` carries this identity. An unreadable descriptor matches
    /// nothing.
    #[must_use]
    pub fn matches(&self, dev: &Device) -> bool {
        let Ok(desc) = dev.device_descriptor() else {
            return false;
        };

        desc.vendor_id() == self.vid
            && desc.product_id() == self.pid
            && (self.serial.as_ref()).is_none_or(|s| read_serial(dev) == *s)
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04x}:{:04x}", self.vid, self.pid)?;
        self.serial
            .as_ref()
            .map_or(Ok(()), |serial| write!(f, ":{serial}"))
    }
}

/// Find the device carrying `id`.
///
/// # Errors
///
/// [`Error::NotFound`] if nothing on the bus matches, [`Error::Usb`] if the bus
/// could not be enumerated.
pub fn find_device(id: &DeviceId) -> Result<Device> {
    rusb::devices()?
        .iter()
        .find(|d| id.matches(d))
        .ok_or_else(|| Error::NotFound { device: id.clone() })
}

/// Wait for a device to appear, polling until `timeout`.
///
/// Polled rather than slept: how long a device takes to come back varies with
/// hub debounce, its own reset and firmware boot, and any enumeration retry,
/// so a fixed sleep is either too short or needlessly long.
///
/// The device is on the bus when this returns, which is not the same as
/// usable: udev may still be applying permissions to its usbfs node, and a
/// composite device's interface drivers may not have bound yet.
///
/// # Errors
///
/// [`Error::NotBack`] if the device did not appear before `timeout`.
pub fn wait_for_device(id: &DeviceId, timeout: Duration) -> Result<Device> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(dev) = find_device(id) {
            return Ok(dev);
        }
        if Instant::now() >= deadline {
            return Err(Error::NotBack { device: id.clone() });
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}
