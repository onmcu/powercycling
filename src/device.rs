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

/// Whether a device carries this identity.
///
/// When `serial` is set to none, it matches any serial number.
pub fn matches_id(dev: &Device, vid: u16, pid: u16, serial: Option<&str>) -> bool {
    let Ok(desc) = dev.device_descriptor() else {
        return false;
    };

    desc.vendor_id() == vid
        && desc.product_id() == pid
        && (serial.is_none_or(|s| read_serial(dev) == s))
}

pub fn find_device(vid: u16, pid: u16, serial: Option<&str>) -> Result<Device> {
    rusb::devices()?
        .iter()
        .find(|d| matches_id(d, vid, pid, serial))
        .ok_or_else(|| Error::NotFound {
            vid,
            pid,
            serial: serial.map(String::from),
        })
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
pub fn wait_for_device(
    vid: u16,
    pid: u16,
    serial: Option<&str>,
    timeout: Duration,
) -> Result<Device> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(dev) = find_device(vid, pid, serial) {
            return Ok(dev);
        }
        if Instant::now() >= deadline {
            return Err(Error::NotBack {
                vid,
                pid,
                serial: serial.map(String::from),
            });
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}
