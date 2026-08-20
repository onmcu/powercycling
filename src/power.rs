//! Finding the ports that feed a device, and switching them together.

use std::time::Duration;

use crate::device::{Device, find_device};
use crate::error::{Error, Result};
use crate::hub::{Hub, USB_SS, all_hubs, open_hub_at};
use crate::port::{self, HubPort};
use crate::sysfs::sysfs_location;

/// Minimum off period when a SuperSpeed port is involved: its power-off is not
/// immediate. Absorbed into the caller's off time rather than added to it.
const SS_POWER_OFF_SETTLE: Duration = Duration::from_millis(200);

/// The device a [`PowerPorts`] was found for.
#[derive(Debug, Clone)]
struct DeviceId {
    vid: u16,
    pid: u16,
    serial: String,
}

/// The ports that must be switched to cut VBUS to one device.
#[derive(Debug, Clone)]
pub struct PowerPorts {
    /// The port feeding the device, on the nearest hub above it that switches
    /// power per port.
    pub primary: HubPort,
    held: Vec<HubPort>,
    device: DeviceId,
}

impl PowerPorts {
    /// Find the ports that must be switched to cut VBUS to `vid:pid:serial`.
    ///
    /// Call this before cutting power: once VBUS drops, the device leaves the
    /// bus and can no longer be looked up by serial.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] if no device matches, [`Error::NoSwitchableHub`] if
    /// nothing above it switches power per port, [`Error::HubUnreadable`] if a
    /// hub in the chain could not be opened, or [`Error::PeerNotSwitchable`] if
    /// the receptacle's other half is ganged.
    pub fn find(vid: u16, pid: u16, serial: &str) -> Result<PowerPorts> {
        let device = find_device(vid, pid, serial)?;
        let bus = device.bus_number();
        let path = device.port_numbers()?;
        let hubs = all_hubs()?;

        let (hub, port) = nearest_switchable_hub(&hubs, bus, &path)?;
        let primary = HubPort::new(hub.clone(), port)?;

        let held = if !hub.may_have_peer() {
            Vec::new()
        } else if let Some((peer_loc, peer_port)) = primary.peer() {
            // The kernel named the peer: hold one port instead of a dozen, and
            // check it is switchable.
            let peer_hub = open_hub_at(&hubs, &peer_loc)?;
            if !peer_hub.per_port_power {
                return Err(Error::PeerNotSwitchable {
                    port: primary.location(),
                    peer: format!("{peer_loc} port {peer_port}"),
                });
            }
            vec![HubPort::new(peer_hub, peer_port)?]
        } else {
            empty_opposite_speed_ports(&hubs, primary.is_super_speed())
        };

        Ok(PowerPorts {
            primary,
            held,
            device: DeviceId {
                vid,
                pid,
                serial: serial.to_string(),
            },
        })
    }

    /// Empty opposite-speed ports held down alongside [`Self::primary`] so the
    /// receptacle's other half cannot keep VBUS alive.
    ///
    /// One entry when a kernel `peer` link named the other half; otherwise
    /// every empty opposite-speed port, which includes it. Empty when the hub
    /// declares USB 2.0 and therefore has no other half.
    #[must_use]
    pub fn held(&self) -> &[HubPort] {
        &self.held
    }

    /// Whether the device is currently absent from the bus.
    #[must_use]
    pub fn is_gone(&self) -> bool {
        let d = &self.device;
        find_device(d.vid, d.pid, &d.serial).is_err()
    }

    /// Switch the held-down ports.
    fn switch_held(&self, on: bool) -> Result<()> {
        // Occupancy was sampled when the ports were found and a caller may
        // reuse `PowerPorts` across cycles, so re-check before cutting.
        // Restoring is unconditional: powering on a live port is a no-op.
        let ports: Vec<&HubPort> = self
            .held
            .iter()
            .filter(|p| on || !p.is_occupied())
            .collect();
        port::usbfs_set_power(&ports, on)
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
            let primary = self.primary.set_power(true);
            self.switch_held(true).and(primary)
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
    /// the off period. Power is restored either way, so a failure never strands
    /// the device or leaves ports held down.
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
        if !self.is_gone() {
            return Err(Error::PowerOffIneffective {
                port: self.primary.location(),
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
fn nearest_switchable_hub(hubs: &[(Device, String)], bus: u8, path: &[u8]) -> Result<(Hub, u8)> {
    for len in (1..=path.len()).rev() {
        let hub = open_hub_at(hubs, &sysfs_location(bus, &path[..len - 1]))?;
        if hub.per_port_power {
            return Ok((hub, path[len - 1]));
        }
    }
    Err(Error::NoSwitchableHub {
        device: sysfs_location(bus, path),
    })
}

/// Every empty port on the opposite-speed half of the bus, grouped by hub.
///
/// The peer hub is never identified. One receptacle holds one device, so the
/// peer of the device's port is necessarily empty; holding down every empty
/// opposite-speed port includes it by construction, and skipping occupied ports
/// leaves other devices untouched.
///
/// Ganged hubs are excluded: clearing `PORT_POWER` on one of their ports can
/// take its neighbours with it.
fn empty_opposite_speed_ports(
    hubs: &[(Device, String)],
    primary_is_super_speed: bool,
) -> Vec<HubPort> {
    hubs.iter()
        .filter(|(dev, _)| {
            dev.device_descriptor()
                .is_ok_and(|d| (d.usb_version() >= USB_SS) != primary_is_super_speed)
        })
        // A hub that cannot be opened is one that could not be switched anyway.
        .filter_map(|(dev, loc)| Hub::open(dev.clone(), loc).ok())
        .filter(|hub| hub.per_port_power)
        .flat_map(|hub| {
            let nports = hub.nports;
            (1..=nports).filter_map(move |port| HubPort::new(hub.clone(), port).ok())
        })
        .filter(|port| !port.is_occupied())
        .collect()
}
