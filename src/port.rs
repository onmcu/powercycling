//! One downstream hub port, and the two ways to switch its power.

use rusb::constants::{LIBUSB_REQUEST_CLEAR_FEATURE, LIBUSB_REQUEST_SET_FEATURE};
use rusb::{DeviceHandle, Direction, GlobalContext, Recipient, RequestType, request_type};
use std::path::{Path, PathBuf};

use crate::TIMEOUT;
use crate::error::{Error, Result};
use crate::hub::Hub;
use crate::sysfs::{SYSFS_USB, split_port_dir};

/// `PORT_POWER` feature selector (USB 2.0 §11.24.2).
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

    /// Human-readable identifier, e.g. `2-1.2.3 port 4`.
    ///
    /// For messages only. It is not a sysfs location and no path can be built
    /// from it; [`Self::child_location`] is the sysfs one.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} port {}", self.hub.location, self.port)
    }

    /// Whether this is the `SuperSpeed` half of a USB 3.x receptacle.
    #[must_use]
    pub fn is_super_speed(&self) -> bool {
        self.hub.is_super_speed()
    }

    /// sysfs location a device plugged into this port would have, e.g.
    /// `2-1.2.3.4`. Everything at or below it loses power with the port.
    #[must_use]
    pub fn child_location(&self) -> String {
        if self.hub.is_root_hub() {
            format!("{}-{}", self.hub.bus, self.port)
        } else {
            format!("{}.{}", self.hub.location, self.port)
        }
    }

    /// Whether something is plugged into this port.
    pub(crate) fn is_occupied(&self) -> bool {
        Path::new(SYSFS_USB).join(self.child_location()).exists()
    }

    /// The other logical port of the same receptacle, if the kernel published a
    /// `peer` link for it.
    pub(crate) fn peer(&self) -> Option<(String, u8)> {
        let target = std::fs::read_link(self.dir.join("peer")).ok()?;
        let (loc, port) = split_port_dir(target.file_name()?.to_str()?)?;
        Some((loc.to_string(), port))
    }

    /// The port's `disable` attribute, if this kernel exposes one (6.0+).
    fn disable_attr(&self) -> Option<PathBuf> {
        let p = self.dir.join("disable");
        p.exists().then_some(p)
    }

    fn switch_failed(&self, usbfs: rusb::Error, sysfs: Option<std::io::Error>) -> Error {
        Error::SwitchFailed {
            port: self.label(),
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

/// Switch several ports over usbfs, opening each hub once.
///
/// usbfs rather than the sysfs `disable` attribute because `disable_store()`
/// sleeps twice the hub's power-on-good delay on every write, and one open
/// dominates the control transfers of the four-odd ports a hub contributes.
///
/// `ports` must be grouped by hub, which is how [`crate::PowerPorts::find`] builds them.
///
/// # Errors
///
/// [`Error::SwitchFailed`] for the first port that could not be switched.
pub fn usbfs_set_power(ports: &[&HubPort], on: bool) -> Result<()> {
    for group in ports.chunk_by(|a, b| a.hub.location == b.hub.location) {
        let [first, ..] = group else { continue };
        let handle = first
            .hub
            .dev
            .open()
            .map_err(|e| first.switch_failed(e, None))?;
        for p in group {
            write_port_power(&handle, p.port, on).map_err(|e| p.switch_failed(e, None))?;
        }
    }
    Ok(())
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

impl std::fmt::Debug for HubPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({})",
            self.label(),
            if self.is_super_speed() { "SS" } else { "HS" }
        )
    }
}
