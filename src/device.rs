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

/// Whether a device carries this identity. An empty `serial` matches any.
pub(crate) fn matches_id(dev: &Device, vid: u16, pid: u16, serial: &str) -> bool {
    let Ok(desc) = dev.device_descriptor() else {
        return false;
    };
    desc.vendor_id() == vid
        && desc.product_id() == pid
        && (serial.is_empty() || read_serial(dev) == serial)
}

pub(crate) fn find_device(vid: u16, pid: u16, serial: &str) -> Result<Device> {
    rusb::devices()?
        .iter()
        .find(|d| matches_id(d, vid, pid, serial))
        .ok_or_else(|| Error::NotFound {
            vid,
            pid,
            serial: serial.to_string(),
        })
}

/// Wait for a device to appear, polling until `timeout`.
///
/// Polled rather than slept because a composite device binds its interfaces in
/// stages and udev adds jitter.
///
/// # Errors
///
/// [`Error::NotBack`] if the device did not appear before `timeout`.
pub fn wait_for_device(vid: u16, pid: u16, serial: &str, timeout: Duration) -> Result<Device> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(dev) = find_device(vid, pid, serial) {
            return Ok(dev);
        }
        if Instant::now() >= deadline {
            return Err(Error::NotBack {
                vid,
                pid,
                serial: serial.to_string(),
            });
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}
