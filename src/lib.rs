#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
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
mod port;
mod power;
mod sysfs;

pub use debug::debug_scan;
pub use device::{Device, DeviceId, wait_for_device};
pub use error::{Error, Result};
pub use port::HubPort;
pub use power::PowerPorts;

/// Timeout for the control transfers this crate issues. They are answered by
/// the hub itself, so anything slower than this is a failure, not congestion.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(1);

/// Find the port(s) associated with the given device, power-cycle them and wait
/// for it to come back.
///
/// The steps must happen in this order: the device cannot be looked up while
/// its VBUS is off.
///
/// The device is identified by a [`DeviceId`]: a `VID`, `PID` and `Serial`
/// triple, where a `None` serial matches on `VID` and `PID` alone.
///
/// # Errors
///
/// Anything [`PowerPorts::find`], [`PowerPorts::cycle`] or [`wait_for_device`]
/// can return.
pub fn power_cycle(device: &DeviceId, off_time: Duration) -> Result<Device> {
    let ports = PowerPorts::find(device)?;
    ports.cycle(off_time)?;
    wait_for_device(device, Duration::from_secs(10))
}
