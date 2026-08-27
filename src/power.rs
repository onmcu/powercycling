//! Finding the ports that feed a device, and switching them together.

use std::time::Duration;

use crate::device::{DeviceId, find_device};
use crate::error::{Error, Result};
use crate::hub::{Hub, Hubs, USB_SS};
use crate::port::HubPort;
use crate::sysfs::sysfs_location;

/// Minimum off period when a `SuperSpeed` port is involved: its power-off is not
/// immediate. Absorbed into the caller's off time rather than added to it.
///
/// This duration could probably be shorter, but 200ms is conservative.
const SS_POWER_OFF_SETTLE: Duration = Duration::from_millis(200);

/// The ports that must be switched to cut VBUS to one device.
#[derive(Debug, Clone)]
pub struct PowerPorts {
    /// The port feeding the device, on the nearest hub above it that switches
    /// power per port.
    primary: HubPort,
    /// The ports held down alongside [`Self::primary`] so the receptacle's
    /// other half cannot keep VBUS alive.
    held: Vec<HubPort>,
    /// The device the ports were found for, kept so [`Self::is_gone`] can look
    /// it up again once its VBUS is off (and the device disconnected).
    device: DeviceId,
}

impl PowerPorts {
    /// Find the ports that must be switched to cut VBUS to `device`.
    ///
    /// Call this before cutting power: once VBUS drops, the device leaves the
    /// bus and can no longer be looked up by serial.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] if no device matches, [`Error::Ambiguous`] if more
    /// than one does, [`Error::NoSwitchableHub`] if no hub above it switches
    /// power per port, [`Error::HubUnreadable`] if a
    /// hub in the chain could not be opened, [`Error::PeerNotSwitchable`] if
    /// the receptacle's other half is ganged, or [`Error::PeerNotFound`] if it
    /// could not be identified.
    pub fn find(device: &DeviceId) -> Result<Self> {
        // Obtain information about where the device is connected
        let dev = find_device(device)?;
        let bus = dev.bus_number();
        let path = dev.port_numbers()?;

        let hubs = Hubs::enumerate()?;

        // Find the nearest hub above bus/path that switches power per port
        let (hub, port) = nearest_switchable_hub(&hubs, bus, &path)?;
        let primary_hub_port = HubPort::new(hub.clone(), port)?;

        // A USB 2.0 hub has no SuperSpeed half, so its receptacles have one port.
        let held = if hub.may_have_peer() {
            peer_ports(&hubs, &primary_hub_port, port)?
        } else {
            Vec::new()
        };

        Ok(Self {
            primary: primary_hub_port,
            held,
            device: device.clone(),
        })
    }

    /// The port feeding the device, on the nearest hub above it that switches
    /// power per port.
    #[must_use]
    pub const fn primary(&self) -> &HubPort {
        &self.primary
    }

    /// The ports held down alongside [`Self::primary`] so the receptacle's
    /// other half cannot keep VBUS alive.
    ///
    /// One port when a kernel `peer` link named it, otherwise every empty
    /// opposite-speed port carrying [`Self::primary`]'s port number, which
    /// includes it (USB 3.2 §10.3.3). Empty when the receptacle has no other
    /// half.
    ///
    /// Without a `peer` link, empty does not prove there is no other half: a
    /// half that switches power in ganged mode is left out rather than
    /// reported, since it cannot be told apart from an unrelated hub. Cutting
    /// then drops the device off the bus with VBUS still up, which
    /// [`Self::cycle`] cannot detect (see [`Error::PowerOffIneffective`]).
    /// Confirm once per hardware setup with an LED or a meter.
    #[must_use]
    pub fn held(&self) -> &[HubPort] {
        &self.held
    }

    /// Whether the device is currently absent from the bus.
    ///
    /// Returns
    /// - `Ok(true)` if the device is not found
    /// - `Ok(false)` if the device is found
    /// - `Err` if an error occurs while searching for the device
    #[must_use]
    pub fn is_gone(&self) -> Result<bool> {
        match find_device(&self.device) {
            // Found means it is not gone
            Ok(_) => Ok(false),
            // Only return true when it was actually not found
            Err(Error::NotFound { .. }) => Ok(true),
            // For all other errors, return that error
            Err(e) => Err(e),
        }
    }

    /// Switch the held-down ports.
    ///
    /// Occupancy was sampled when the ports were found and a caller may reuse
    /// `PowerPorts` across cycles, so re-check before cutting. Restoring is
    /// unconditional: powering on a live port is a no-op.
    ///
    /// Cutting stops at the first failure, since the cut is already void.
    /// Restoring tries every port regardless and reports the first failure, so
    /// one bad port never leaves the others held down.
    fn switch_held(&self, on: bool) -> Result<()> {
        let mut ports = self.held.iter().filter(|p| on || !p.is_occupied());
        if on {
            ports.map(|p| p.set_power(true)).fold(Ok(()), Result::and)
        } else {
            ports.try_for_each(|p| p.set_power(false))
        }
    }

    /// Switch the device's port, holding the receptacle's other half down for
    /// as long as it is off.
    ///
    /// Does not verify the effect; [`Self::cycle`] does.
    ///
    /// # Errors
    ///
    /// [`Error::SwitchFailed`] if any of the ports could not be switched.
    pub fn set_power(&self, on: bool) -> Result<()> {
        if on {
            // Restore the primary first, then release the held-down ports.
            // Both run whatever the other does; the first failure is reported.
            let primary = self.primary.set_power(true);
            let held = self.switch_held(true);
            primary.and(held)
        } else {
            self.switch_held(false)?;
            self.primary.set_power(false)
        }
    }

    /// Cut power to the device, wait `off_time`, then restore it.
    ///
    /// # Errors
    ///
    /// [`Error::PowerOffIneffective`] if the device is still enumerated after
    /// the off period, or [`Error::SwitchFailed`] if a port would not switch.
    /// Power is restored either way, so a failure never strands the device or
    /// leaves ports held down.
    pub fn cycle(&self, off_time: Duration) -> Result<()> {
        let outcome = self.cut_and_wait(off_time);
        let restored = self.set_power(true);
        outcome.and(restored)
    }

    fn cut_and_wait(&self, off_time: Duration) -> Result<()> {
        self.set_power(false)?;

        let super_speed_involved = self.primary.is_super_speed() || !self.held.is_empty();
        std::thread::sleep(if super_speed_involved {
            off_time.max(SS_POWER_OFF_SETTLE)
        } else {
            off_time
        });

        // Check before restoring power, while the evidence is still there.
        if !self.is_gone()? {
            return Err(Error::PowerOffIneffective {
                port: self.primary.to_string(),
            });
        }
        Ok(())
    }
}

/// The nearest hub above `bus`-`path` that switches power per port, and the
/// port of it leading down to the device.
///
/// Hubs chained behind a capable one commonly report ganged switching, where
/// clearing `PORT_POWER` disconnects the port without dropping VBUS, so the
/// device's immediate parent is often the wrong hub.
///
/// The walk stops below the root hub. Root hub ports are host controller ports,
/// which the specification's hub chapter does not cover (USB 3.2 §10.1) - in
/// particular nothing relates the two root hubs' port numbers, so the other
/// half of such a receptacle cannot be identified.
fn nearest_switchable_hub(hubs: &Hubs, bus: u8, path: &[u8]) -> Result<(Hub, u8)> {
    for len in (2..=path.len()).rev() {
        let hub = hubs.open_at(&sysfs_location(bus, &path[..len - 1]))?;
        if hub.per_port_power {
            return Ok((hub, path[len - 1]));
        }
    }
    Err(Error::NoSwitchableHub {
        device: sysfs_location(bus, path),
    })
}

/// The ports on the other half of the receptacle `primary` feeds, which have to
/// be cut with it: VBUS stays on while either half asks for it (USB 3.2 §10.1,
/// Table 10-2).
///
/// The kernel names the port outright where it publishes a `peer` link. Where
/// it does not, the peer carries the same port number as `primary`, because
/// both halves of a hub number their downstream ports alike (§10.3.3) - but
/// which hub is the other half cannot be derived. Nothing relates the two root
/// hubs' port numbers (§10.1), and a chain can enter the two buses at different
/// depths, so the halves of one hub need not sit at the same port path: an
/// RTS5411 whose USB 2.0 half hangs off a USB 2.0-only hub at `2-1.2` has its
/// `SuperSpeed` half directly on a root port at `3-2`.
///
/// So the port number is used as the filter and the hub is left unidentified:
/// every empty port of that number, on an opposite-speed hub that switches
/// power per port, is held. The peer is among them by construction, and empty
/// ports feed nothing, so the ones that are not the peer cost nothing.
///
/// Empty when the receptacle has no other half to hold. A half that is not on
/// the bus holds no `PORT_POWER`, so VBUS follows the half that is
/// (Table 10-2).
///
/// # Errors
///
/// [`Error::PeerNotSwitchable`] if the kernel named a ganged peer,
/// [`Error::PeerNotFound`] if it named an occupied one, or
/// [`Error::HubUnreadable`] if the named hub could not be opened.
fn peer_ports(hubs: &Hubs, primary: &HubPort, port: u8) -> Result<Vec<HubPort>> {
    let Some((location, peer)) = primary.peer() else {
        return Ok(numbered_opposite_speed_ports(
            hubs,
            primary.is_super_speed(),
            port,
        ));
    };

    let peer_hub = hubs.open_at(&location)?;
    if !peer_hub.per_port_power {
        return Err(Error::PeerNotSwitchable {
            port: primary.to_string(),
            peer: format!("{location} port {peer}"),
        });
    }

    let peer = HubPort::new(peer_hub, peer)?;
    // One receptacle holds one device, so a named peer must read empty. If it
    // does not, the link does not mean what this crate takes it to mean, and
    // cutting the port would strand a device.
    if peer.is_occupied() {
        return Err(Error::PeerNotFound {
            port: primary.to_string(),
            candidate: peer.to_string(),
        });
    }
    Ok(vec![peer])
}

/// Every empty port numbered `port` on an opposite-speed hub that switches
/// power per port.
///
/// Ganged hubs are excluded: clearing `PORT_POWER` on one of their ports can
/// take its neighbours with it. Root hubs are too - the other half of an
/// external hub's receptacle is that hub's own other half, never a host
/// controller port.
fn numbered_opposite_speed_ports(
    hubs: &Hubs,
    primary_is_super_speed: bool,
    port: u8,
) -> Vec<HubPort> {
    hubs.iter()
        .filter(|dev| dev.port_numbers().is_ok_and(|path| !path.is_empty()))
        .filter(|dev| {
            dev.device_descriptor()
                .is_ok_and(|d| (d.usb_version() >= USB_SS) != primary_is_super_speed)
        })
        // A hub that cannot be opened is one that could not be switched anyway.
        .filter_map(|dev| Hub::open(dev.clone()).ok())
        .filter(|hub| hub.per_port_power && port <= hub.nports)
        .filter_map(|hub| HubPort::new(hub, port).ok())
        .filter(|peer| !peer.is_occupied())
        .collect()
}
