//! Power-cycle a USB device identified by `vid:pid:serial`, by switching hub
//! port power. Linux only.
//!
//! ```no_run
//! # use std::time::Duration;
//! powercycling::power_cycle(0x0483, 0x374e, "0050003A3233511639363634", Duration::from_secs(2))?;
//! # Ok::<(), powercycling::Error>(())
//! ```
//!
//! For finer control, [`PowerPorts::find`] returns the ports and
//! [`PowerPorts`] switches them.
//!
//! # How it works
//!
//! [`PowerPorts::find`] walks up from the device to the nearest hub that does
//! per-port power switching (PPPS) and cuts `PORT_POWER` on the port leading to
//! it. Hubs chained behind a capable one commonly report ganged switching,
//! where clearing `PORT_POWER` disconnects the port without dropping VBUS, so
//! the device's immediate parent is often the wrong hub.
//!
//! A USB 3.x receptacle is one socket carrying two links, exposed as two
//! logical hubs. A device trains only one of them, but VBUS is gated on
//! `PORT_POWER` of *both*, so cutting the half a device sits on drops it off
//! the bus while leaving it powered. The other half must be held down too.
//!
//! That half is found without identifying the hub it belongs to. One receptacle
//! holds one device, so the peer of an occupied port is necessarily empty;
//! holding down every empty opposite-speed port therefore includes it, and
//! skipping occupied ports means no other device is disturbed. Where the kernel
//! publishes a `peer` link it names the port exactly and only one is held.
//!
//! # Requirements
//!
//! Switching a port needs write access to either the port's sysfs `disable`
//! attribute (kernel 6.0+) or the hub's usbfs node. See uhubctl's
//! `udev/rules.d/52-usb.rules`.
//!
//! # Errors
//!
//! Nothing here succeeds silently. [`Error`] distinguishes a missing device, a
//! chain with no switchable hub, a hub that could not be opened, a ganged peer,
//! and a device still enumerated after power-off.

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
pub use device::{Device, wait_for_device};
pub use error::{Error, Result};
pub use port::HubPort;
pub use power::PowerPorts;

/// Timeout for the control transfers this crate issues. They are answered by
/// the hub itself, so anything slower than this is a failure, not congestion.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(1);

/// Find the device's ports, power-cycle them, and wait for it to come back.
///
/// The steps must happen in this order: the device cannot be looked up while
/// its VBUS is off.
///
/// # Errors
///
/// Anything [`PowerPorts::find`], [`PowerPorts::cycle`] or [`wait_for_device`]
/// can return.
pub fn power_cycle(vid: u16, pid: u16, serial: &str, off_time: Duration) -> Result<Device> {
    let ports = PowerPorts::find(vid, pid, serial)?;
    ports.cycle(off_time)?;
    wait_for_device(vid, pid, serial, Duration::from_secs(10))
}
