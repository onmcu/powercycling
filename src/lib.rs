//! Power-cycle a USB device identified by VID (Vendor ID), PID (Product ID)
//! and serial number, by switching hub port power (PPPS). Linux only.
//!
//! Built for hardware-in-the-loop rigs, where a hanging MCU devboard needs its
//! VBUS cut rather than only the USB link reset. The crate cuts exactly the one
//! port that feeds the device. For USB 3.x hubs, it also holds the other half of
//! the receptacle down with it.
//!
//! When it cannot be ensured that only your device loses power, an [`Error`]
//! with precise instructions is returned.
//!
//! # 1. Cycle a device
//!
//! **Hardware:** the device hangs off a hub that does per-port power switching
//! (PPPS). Most bare-board hubs do; many consumer hubs _claim_ it but do not
//! have the hardware (MOSFETs) to actually cut the power (step 2 finds that out).
//!
//! **Permissions:** switching a port needs write access to the port's sysfs
//! `disable` attribute (kernel 6.0+) or the hub's usbfs node.
//! Install uhubctl's [`52-usb.rules`](https://github.com/mvp/uhubctl/blob/master/udev/rules.d/52-usb.rules)
//! and re-plug the hub. A missing rule shows up as [`Error::HubUnreadable`].
//!
//! **Identity:** `lsusb` gives VID and PID; the serial is in
//! `/sys/bus/usb/devices/<location>/serial`.
//! The crate can work without a serial if the VID-PID pair is unique on the bus.
//!
//! ```rust,no_run
//! use powercycling::{DeviceId, HubPairs};
//! use std::time::Duration;
//!
//! fn main() -> Result<(), powercycling::Error> {
//!     let device = DeviceId::new(0x0483, 0x374e, Some("0050003A3233511639363634"));
//!     // Off for 2 s, then up to 10 s for the device to re-enumerate. `HubPairs`
//!     // is what your machine needs declared about its hubs. Usually nothing, but
//!     // step 3 tells you if it does.
//!     powercycling::power_cycle(
//!         &device,
//!         &HubPairs::none(),
//!         Duration::from_secs(2),
//!         Duration::from_secs(10),
//!     )?;
//!     Ok(())
//! }
//! ```
//!
//! The same from the command line, with the `powercycling` binary
//! (`cargo install powercycling`):
//!
//! ```text
//! powercycling 0483 374e 0050003A3233511639363634
//! ```
//!
//! # 2. Check it once per setup
//!
//! `Ok` means the device left the bus and came back. It does **not** (and
//! cannot!) prove the device lost power: a port that is switched off disables
//! its link, so the device disappears, no matter if VBUS really dropped or not.
//! Nothing over USB tells the two apart.
//!
//! So do this check once with a device that has an LED lighting up when the USB
//! port is powered and confirm visually that power is actually being cut:
//!
//! ```text
//! powercycling <vid> <pid> <serial> --verify
//! ```
//!
//! `--verify` cuts the power, reports what left the bus (flagging anything that
//! was not your device as **COLLATERAL**) and confirms everything came back.
//!
//! While it runs, watch the LED:
//!
//! | LED during `--verify` | meaning | do |
//! |---|---|---|
//! | dark, and no collateral | power really was cut | done |
//! | dark, collateral listed | something else lost power too | step 3, [`BehindHub`](Error::BehindHub) |
//! | stays lit | the port was disabled, VBUS was not | step 3, "LED stays lit" |
//!
//! Once a setup passes this, it stays passed: the pairing is derived from the
//! topology, not from guesswork.
//!
//! # 3. When it does not work
//!
//! Every failure is an [`Error`] that names the reason.
//!
//! The following is the escalation ladder, from "fix the call" to "fix the
//! hardware".
//!
//! - **[`NotFound`](Error::NotFound) / [`Ambiguous`](Error::Ambiguous):** nothing
//!   or several devices match. Check `lsusb` or `powercycling --tree`. If
//!   necessary, add the serial.
//!
//! - **[`HubUnreadable`](Error::HubUnreadable):** the hub does not open. Likely
//!   permissions, see step 1.
//!
//! - **[`NoSwitchableHub`](Error::NoSwitchableHub):** the device is plugged
//!   directly into the machine. Root hub ports are host controller ports and
//!   therefore outside the specification's hub chapter, so the crate does not
//!   switch them (yet).
//!   _Fix:_ Put a PPPS hub between the machine and the device.
//!
//! - **[`BehindHub`](Error::BehindHub):** the device's direct "ancestor hub" does
//!   not switch power per port (chained hubs usually report _ganged_). Above that
//!   hub sits a port that _does_ switch, but it may feed other devices. The crate
//!   refuses to take every device connected to that hub down on your behalf. The
//!   error names the hub and how many levels up it is; the binary prints the
//!   exact `--above N` to add.
//!
//!   _Fix:_
//!
//!   - put the device on a PPPS hub directly, or
//!   - if that hub _is_ the unit you want cycled, e.g., a carrier board with its
//!     own hub, a devboard and measurement hardware on it, explicitly tell the
//!     crate how many hubs to go up with [`PowerPorts::find_above`]:
//!
//!     ```rust,no_run
//!     use powercycling::{DeviceId, HubPairs, PowerPorts};
//!     use std::time::Duration;
//!
//!     fn main() -> Result<(), powercycling::Error> {
//!         let mcu = DeviceId::new(0x0483, 0x374e, Some("0050003A3233511639363634"));
//!         let pairs = HubPairs::none();
//!         // The MCU alone (if its hub does PPPS) ...
//!         PowerPorts::find(&mcu, &pairs)?.cycle(Duration::from_secs(2))?;
//!         // ... or the whole carrier: the hub one level up, with everything on it.
//!         PowerPorts::find_above(&mcu, 1, &pairs)?.cycle(Duration::from_secs(2))?;
//!         powercycling::wait_for_device(&mcu, Duration::from_secs(10))?;
//!         Ok(())
//!     }
//!     ```
//!
//!     For the same behavior on the command line, add `--above 1`.
//!     Naming the carrier through the MCU's serial is deliberate: identical
//!     carriers may have identical hubs with no serials of their own, so the
//!     hub's VID/PID alone would be [`Ambiguous`](Error::Ambiguous).
//!
//! - **[`HubUnpaired`](Error::HubUnpaired):** the device's hub is one half of a
//!   USB 3.x hub, and the crate cannot tell which hub is the other half. Both
//!   halves keep the receptacle powered, so cutting one alone would only
//!   disconnect the device. The crate cuts nothing. This happens on boards that
//!   route the two halves in a way the crate's rules (documented on
//!   [`HubPairs`]) do not recognize.
//!
//!   The fix is one declaration, usually only necessary once per machine:
//!
//!   1. Check the tree ([`tree`]): which hub is paired with which, and why this
//!      one is not:
//!      ```text
//!      powercycling --tree
//!      ```
//!   2. Plug something with a power LED into the unpaired hub and find its
//!      other half ([`probe`]):
//!      ```text
//!      powercycling --probe 2-1.2 4
//!      ```
//!      The probe prints its plan: first, cycle the port alone, then together
//!      with each candidate, most likely pair first.
//!      Then it runs the plan one step per `Enter`-press: cut, wait, restore,
//!      then ask "LED went dark?". Press `y` when it did; the probe prints the
//!      line to declare.
//!   3. Declare it. The crate reads no file on its own; keep the pairs wherever
//!      your configuration lives and pass them in:
//!      ```text
//!      # cm5.pairs - one line per pair of hubs that share receptacles
//!      2-1 usb3
//!      ```
//!      ```rust,no_run
//!      use powercycling::HubPairs;
//!
//!      fn main() -> Result<(), powercycling::Error> {
//!          let from_file = HubPairs::load("cm5.pairs")?;
//!          let from_text: HubPairs = "2-1 usb3".parse()?;
//!          let built = HubPairs::none().pair("2-1", "usb3");
//!          assert_eq!(from_file, from_text);
//!          assert_eq!(from_text, built);
//!          Ok(())
//!      }
//!      ```
//!      The binary takes `--pairs-file cm5.pairs`.
//!      One line covers every hub chained below the pair, no matter how many
//!      identical ones there are. If a hub's receptacles have _no other half_ at
//!      all, declare it as `<hub> none`.
//!
//! - **[`PeerNotSwitchable`](Error::PeerNotSwitchable):** the other half was
//!   identified, but switches power in ganged mode, so it cannot be cut without
//!   taking its neighbours. This receptacle cannot be power-cycled per port.
//!   _Fix:_ use a different hub.
//!
//! - **[`PeerNotFound`](Error::PeerNotFound):** the peer's port holds a device of
//!   its own, which one receptacle cannot (unless it is another hub). The pairing
//!   is probably wrong: check `--tree`, and declare the right pairing as
//!   described above.
//!
//! - **[`PowerOffIneffective`](Error::PowerOffIneffective):** the port was
//!   switched off, but the device is still enumerated. The hub accepted
//!   `PORT_POWER`, but did not act on it.
//!   _Fix:_ use a hub that does.
//!
//! - **LED stays lit, but `Ok`:** the port was disabled and VBUS was not cut.
//!   Either the hub does not gate VBUS on that port (measure), or the peer's port
//!   is not held down properly. To tell them apart:
//!
//!   ```text
//!   powercycling <vid> <pid> <serial> --primary-only
//!   ```
//!
//!   which deliberately cuts the device's port alone. LED dark ⇒ this hub gates
//!   VBUS on one port and the other half never mattered. LED lit ⇒ the other half
//!   matters, and the pairing (`--tree`) is what to look at.
//!
//! - **Anything else:** `--debug` ([`debug_scan`]) prints what every stage of the
//!   search sees.
//!
//! # Finer control
//!
//! [`PowerPorts::find`] returns the ports and [`PowerPorts`] switches them;
//! [`PowerPorts::find_above`] does the same for the hub N levels above the
//! device:
//!
//! ```rust,no_run
//! use powercycling::{DeviceId, HubPairs, PowerPorts};
//! use std::{thread::sleep, time::Duration};
//!
//! fn main() -> Result<(), powercycling::Error> {
//!     let ports = PowerPorts::find(&DeviceId::new(0x0483, 0x374e, None), &HubPairs::none())?;
//!     ports.set_power(false)?; // hold the device off
//!     sleep(Duration::from_secs(2));
//!     ports.set_power(true)?;
//!     Ok(())
//! }
//! ```
//!
//! Find the ports _before_ cutting: once VBUS drops, the device can no longer be
//! looked up. [`PowerPorts::cycle`] cuts, waits, checks that the device is gone,
//! and restores. It restores on failure, too, so no port is left held down. If a
//! `SuperSpeed` port is involved, the off time is at least 200 ms, since such a
//! port's power-off is not immediate.
//!
//! # How it works
//!
//! Only a hub that switches power per port can cut VBUS. A _ganged_ hub still
//! accepts `PORT_POWER`, but only disables the port: the device disappears from
//! the bus, VBUS stays on. Hubs chained behind a PPPS hub usually report ganged,
//! so [`PowerPorts::find`] refuses with [`Error::BehindHub`] rather than cutting
//! the port above, which feeds every device on that hub.
//!
//! A USB 3.x receptacle is fed by two logical hubs, a USB 2.0 and a `SuperSpeed`
//! one, and its VBUS stays on while either has its port powered (USB 3.2 §10.1,
//! Table 10-2). So the crate cuts the device's port and holds port N of the other
//! half down with it. Which hub is the other half is derived from the topology,
//! or declared where it cannot be. The rules, a real tree and what they cannot
//! cover are documented on [`HubPairs`].
//!
//! The approach, walking to a PPPS-capable hub and switching both halves of a
//! USB 3.x receptacle, comes from [uhubctl](https://github.com/mvp/uhubctl) by
//! Vadim Mikhailov.
#[cfg(not(target_os = "linux"))]
compile_error!(
    "powercycling is Linux-only: it depends on sysfs USB port devices \
     (`disable`) and hub attributes, which no other platform provides"
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
mod tree;

/// The USB library this crate is built on. [`Device`] and [`Error::Usb`] are
/// its types, so it is re-exported to spare callers a matching dependency.
pub use rusb;

pub use debug::debug_scan;
pub use device::{Device, DeviceId, wait_for_device};
pub use error::{Error, Result};
pub use pairing::HubPairs;
pub use port::HubPort;
pub use power::PowerPorts;
pub use probe::probe;
pub use tree::tree;

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
