#![doc = include_str!("../README.md")]
#[cfg(not(target_os = "linux"))]
compile_error!(
    "powercycling is Linux-only: it depends on sysfs USB port devices \
     (`peer`, `disable`), which no other platform provides"
);

use std::time::Duration;

mod debug;
mod device;
mod error;
mod hub;
mod pairing;
mod port;
mod power;
mod probe;
mod sysfs;

/// The USB library this crate is built on. [`Device`] and [`Error::Usb`] are
/// its types, so it is re-exported to spare callers a matching dependency.
pub use rusb;

pub use debug::{debug_scan, pairing_report};
pub use device::{Device, DeviceId, wait_for_device};
pub use error::{Error, Result};
pub use pairing::HubPairs;
pub use port::HubPort;
pub use power::PowerPorts;
pub use probe::probe;

/// Timeout for the control transfers this crate issues. They are answered by
/// the hub itself, so anything slower than this is a failure, not congestion.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(1);

/// Find the port(s) associated with the given device, power-cycle them and wait
/// up to `back_timeout` for it to re-enumerate.
///
/// The steps must happen in this order: the device cannot be looked up while
/// its VBUS is off.
///
/// The device is identified by a [`DeviceId`]: a `VID`, `PID` and `Serial`
/// triple, where a `None` serial matches on `VID` and `PID` alone. `pairs`
/// declares which hubs share receptacles where the bus cannot tell -
/// [`HubPairs::none`] on a board that needs nothing declared.
///
/// # Errors
///
/// Anything [`PowerPorts::find`], [`PowerPorts::cycle`] or [`wait_for_device`]
/// can return.
pub fn power_cycle(
    device: &DeviceId,
    pairs: &HubPairs,
    off_time: Duration,
    back_timeout: Duration,
) -> Result<Device> {
    let ports = PowerPorts::find(device, pairs)?;
    ports.cycle(off_time)?;
    wait_for_device(device, back_timeout)
}
