//! Finding the ports that feed a device, and switching them together.

use std::time::Duration;

use crate::device::{DeviceId, find_device};
use crate::error::{Error, Result};
use crate::hub::{Hub, Hubs};
use crate::pairing::{HubPairs, Pairing, Verdict};
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
    /// The port feeding the device, on the hub directly above it.
    primary: HubPort,
    /// The port on the other half of the receptacle, held down alongside
    /// [`Self::primary`] so it cannot keep VBUS alive. `None` when the
    /// receptacle has no other half.
    held: Option<HubPort>,
}

impl PowerPorts {
    /// Find the ports that must be switched to cut VBUS to `device`.
    ///
    /// `pairs` declares which hubs share receptacles where the bus cannot
    /// tell - [`HubPairs::none`] on a board that needs nothing declared.
    ///
    /// Call this before cutting power: once VBUS drops, the device leaves the
    /// bus and can no longer be looked up by serial.
    ///
    /// Only the device's own port is ever cut. If the hub it hangs off does
    /// not switch power per port, the port that does sits further up and
    /// feeds that hub whole; `find` refuses with [`Error::BehindHub`] rather
    /// than cutting every device on the hub. To cycle a hub and everything on
    /// it, name the hub.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] if no device matches, [`Error::Ambiguous`] if more
    /// than one does, [`Error::NoSwitchableHub`] if no hub above it switches
    /// power per port, [`Error::BehindHub`] if one does but only above a hub
    /// that does not, [`Error::HubUnreadable`] if a hub in the chain could not
    /// be opened, [`Error::HubUnpaired`] if the receptacle's other half could
    /// not be identified, [`Error::PeerNotSwitchable`] if it is ganged, or
    /// [`Error::PeerNotFound`] if it holds a device of its own.
    pub fn find(device: &DeviceId, pairs: &HubPairs) -> Result<Self> {
        Self::find_above(device, 0, pairs)
    }

    /// Find the ports that cut VBUS to the hub `levels` hubs above `device` -
    /// and so to everything on that hub, `device` included.
    ///
    /// For a carrier board with its own hub, a devboard and measurement
    /// hardware on it: `find(mcu)` cycles the MCU alone, `find_above(mcu, 1)`
    /// cycles the whole carrier. Naming the carrier by the MCU's serial is
    /// what makes this work when several identical carriers - identical hubs,
    /// no serials of their own - hang off one machine.
    ///
    /// `levels == 0` is [`Self::find`].
    ///
    /// # Errors
    ///
    /// As [`Self::find`], plus [`Error::NothingAbove`] if there are fewer
    /// than `levels` hubs between the device and the root hub.
    pub fn find_above(device: &DeviceId, levels: u8, pairs: &HubPairs) -> Result<Self> {
        // Obtain information about where the device is connected
        let dev = find_device(device)?;
        let bus = dev.bus_number();
        let mut path = dev.port_numbers()?;

        // Climb: everything in the path above the device is a hub. The root
        // hub is not a target, so at least one port must remain.
        let keep = path
            .len()
            .checked_sub(usize::from(levels))
            .filter(|&keep| keep > 0)
            .ok_or_else(|| Error::NothingAbove {
                device: sysfs_location(bus, &path),
                levels,
            })?;
        path.truncate(keep);

        let hubs = Hubs::enumerate()?;

        // The hub directly above bus/path, which has to switch power per port
        let (hub, port) = switchable_parent(&hubs, bus, &path)?;
        let primary = HubPort::new(hub, port)?;

        let pairing = Pairing::compute(&hubs, pairs);
        let held = peer_port(&hubs, &pairing, &primary)?;

        Ok(Self { primary, held })
    }

    /// The port feeding the device, on the hub directly above it.
    #[must_use]
    pub const fn primary(&self) -> &HubPort {
        &self.primary
    }

    /// The port on the other half of the receptacle, held down alongside
    /// [`Self::primary`] so it cannot keep VBUS alive: the same port number on
    /// the hub paired with [`Self::primary`]'s (USB 3.2 §10.3.3). Empty when
    /// the receptacle has no other half.
    ///
    /// When the device is itself a hub, its other half occupies this port
    /// (§10.1) and is cut with it.
    #[must_use]
    pub const fn held(&self) -> &[HubPort] {
        self.held.as_slice()
    }

    /// Whether the device is currently absent from the bus.
    ///
    /// Read from sysfs at the device's location - it is the direct child of
    /// [`Self::primary`] - rather than looked up by identity, so it works for
    /// a target that shares its `vid:pid` with others, such as one of several
    /// identical hubs.
    #[must_use]
    pub fn is_gone(&self) -> bool {
        !self.primary.is_occupied()
    }

    /// Switch the held-down port, if any.
    ///
    /// Occupancy was sampled when the ports were found and a caller may reuse
    /// `PowerPorts` across cycles, so re-check before cutting. Restoring is
    /// unconditional: powering on a live port is a no-op.
    fn switch_held(&self, on: bool) -> Result<()> {
        match &self.held {
            Some(held) if on || held.is_holdable_for(&self.primary) => held.set_power(on),
            _ => Ok(()),
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

        let super_speed_involved = self.primary.is_super_speed() || self.held.is_some();
        std::thread::sleep(if super_speed_involved {
            off_time.max(SS_POWER_OFF_SETTLE)
        } else {
            off_time
        });

        // Check before restoring power, while the evidence is still there.
        if !self.is_gone() {
            return Err(Error::PowerOffIneffective {
                port: self.primary.to_string(),
            });
        }
        Ok(())
    }
}

/// What is cut and what is held: `cutting 2-1.4 port 1 (HS), holding down
/// 3-4 port 1 (SS)`.
impl std::fmt::Display for PowerPorts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cutting {:?}", self.primary)?;
        match &self.held {
            Some(held) => write!(f, ", holding down {held:?}"),
            None => write!(f, " (receptacle has no other half)"),
        }
    }
}

/// The hub directly above `bus`-`path`, which has to switch power per port,
/// and the port of it leading down to the device.
///
/// Hubs chained behind a capable one commonly report ganged switching, where
/// clearing `PORT_POWER` disconnects the port without dropping VBUS. The port
/// that does cut VBUS is then further up and feeds the chained hub whole, so
/// cutting it takes every device on that hub. That is refused rather than done
/// on the caller's behalf: [`Error::BehindHub`] names the hub, and naming it as
/// the device cycles all of it deliberately.
///
/// The walk stops below the root hub. Root hub ports are host controller ports,
/// which the specification's hub chapter does not cover (USB 3.2 §10.1).
fn switchable_parent(hubs: &Hubs, bus: u8, path: &[u8]) -> Result<(Hub, u8)> {
    for len in (2..=path.len()).rev() {
        let hub = hubs.open_at(&sysfs_location(bus, &path[..len - 1]))?;
        if !hub.per_port_power {
            continue;
        }
        if len == path.len() {
            return Ok((hub, path[len - 1]));
        }
        // The nearest switchable port feeds a chained hub, not the device.
        let chained = sysfs_location(bus, &path[..len]);
        let hub_id = hubs
            .device_at(&chained)
            .and_then(|dev| DeviceId::of(dev).ok());
        return Err(Error::BehindHub {
            device: sysfs_location(bus, path),
            hub: chained,
            hub_id,
            levels: u8::try_from(path.len() - len).unwrap_or(u8::MAX),
        });
    }
    Err(Error::NoSwitchableHub {
        device: sysfs_location(bus, path),
    })
}

/// The port on the other half of the receptacle `primary` feeds, which has to
/// be cut with it: VBUS stays on while either half asks for it (USB 3.2 §10.1,
/// Table 10-2).
///
/// It is the same port number on the hub paired with `primary`'s, because
/// both halves of a hub number their downstream ports alike (§10.3.3). Which
/// hub that is comes from [`Pairing`].
///
/// `None` when the receptacle has no other half to hold: the hub is a USB 2.0
/// hub or declared alone, or the declared partner has no port of that number.
///
/// # Errors
///
/// [`Error::HubUnpaired`] if the hub has another half that could not be
/// identified, [`Error::PeerNotSwitchable`] if that half is ganged,
/// [`Error::PeerNotFound`] if its port holds a device of its own, or
/// [`Error::HubUnreadable`] if it could not be opened.
fn peer_port(hubs: &Hubs, pairing: &Pairing, primary: &HubPort) -> Result<Option<HubPort>> {
    let hub = primary.hub();
    let other = match pairing.verdict(&hub.location) {
        Verdict::Paired { other, .. } => other,
        Verdict::DeclaredAlone | Verdict::Usb2Hub => return Ok(None),
        // A root hub is never a primary, and a hub `Pairing` does not know
        // is one whose descriptor it could not read; neither can be paired.
        Verdict::StoodInFor { .. } | Verdict::Unpaired | Verdict::Unknown => {
            return Err(Error::HubUnpaired {
                port: primary.to_string(),
                hub: hub.location.clone(),
                other_side: hub.other_side(),
            });
        }
    };

    let port = primary.port();
    let peer_hub = hubs.open_at(other)?;
    // A declared partner may be smaller than the hub - a root hub with fewer
    // ports, say. A receptacle beyond its last port has one half only.
    if port > peer_hub.nports {
        return Ok(None);
    }
    if !peer_hub.per_port_power {
        return Err(Error::PeerNotSwitchable {
            port: primary.to_string(),
            peer: format!("{other} port {port}"),
        });
    }

    let peer = HubPort::new(peer_hub, port)?;
    if !peer.is_holdable_for(primary) {
        return Err(Error::PeerNotFound {
            port: primary.to_string(),
            candidate: peer.to_string(),
        });
    }
    Ok(Some(peer))
}
