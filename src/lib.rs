//! Power-cycle a USB device (VID, PID, Serial) by switching hub port power.
//! Linux only. Only for hubs that support proper per-port power switching (PPPS).
//!
//! ```no_run
//! # use std::time::Duration;
//! powercycling::power_cycle(0x0483, 0x374e, Some("0050003A3233511639363634"), Duration::from_secs(2))?;
//! # Ok::<(), powercycling::Error>(())
//! ```
//!
//! For finer control, [`PowerPorts::find`] returns the ports and
//! [`PowerPorts`] switches them.
//!
//! # How it works
//!
//! [`PowerPorts::find`] walks up from the device to the nearest hub that does
//! per-port power switching (PPPS) and cuts `PORT_POWER` on its port. A hub
//! without switches (_ganged_) still *accepts* `PORT_POWER` and answers it by
//! disabling the port, so the device vanishes from the bus with VBUS untouched,
//! i.e., a power cycle that cuts no power. Skipping those hubs is the point of
//! the walk.
//!
//! The switched port can be several levels above the device, so the cut takes
//! the whole branch below it. That is, _all devices_ under the skipped hubs.
//!
//! ## USB 3.x Handling
//! A USB 3.x receptacle is one physical socket carrying two USB links:
//! a `SuperSpeed` hub and a USB 2.0 hub each own a port.
//! A device connected to the physical socket occupies only one of these ports
//! (either USB 2 or USB 3) and leaves the other reading empty.
//!
//! However, the socket only has a single VBUS pin and its two ports feed it
//! like switches wired in parallel: if either port still has `PORT_POWER` set,
//! the device stays powered.
//! Cutting only the connected half (e.g., the USB 2.0 port for a USB 2.0 device)
//! drops the device off the bus with its VBUS untouched. That is the same
//! silent no-op as a ganged hub. **Both halves have to be down at once.**
//!
//! Now comes the tricky part: Which port is that other half?
//! When the kernel publishes a `peer` link, sysfs names it outright and that
//! one port is held.
//! When no `peer` link is published, this crate rather "covers" that "identifies"
//! the port with the following procedure: every *empty* port of the opposite speed
//! is held down. The peer _must_ be among them, because the device occupies its own
//! half of the receptacle and so leaves the other half empty.
//! Occupied ports are skipped, so no other device is disturbed.
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

/// Find the port(s) associated with the given device, power-cycle them and wait
/// for it to come back.
///
/// The steps must happen in this order: the device cannot be looked up while
/// its VBUS is off.
///
/// The device is identified by a `VID`, `PID` and `Serial` triple.
///
/// If `serial` is `None`, the device is only matched by `VID` and `PID`.
///
/// # Errors
///
/// Anything [`PowerPorts::find`], [`PowerPorts::cycle`] or [`wait_for_device`]
/// can return.
pub fn power_cycle(vid: u16, pid: u16, serial: Option<&str>, off_time: Duration) -> Result<Device> {
    let ports = PowerPorts::find(vid, pid, serial)?;
    ports.cycle(off_time)?;
    wait_for_device(vid, pid, serial, Duration::from_secs(10))
}
