//! Why a power cycle could not be carried out.

use crate::device::DeviceId;

/// Why a power cycle could not be carried out.
#[derive(Debug)]
pub enum Error {
    /// Nothing on the bus matches this `vid:pid:serial`.
    NotFound {
        /// The identity that was searched for.
        device: DeviceId,
    },
    /// No hub between the device and the root hub does per-port power
    /// switching, so nothing can cut its VBUS.
    ///
    /// Root hubs are not considered: they are host controller ports, which the
    /// hub chapter of the specification does not cover (USB 3.2 §10.1, "Host
    /// controller ports may have different requirements").
    NoSwitchableHub {
        /// sysfs location of the device, e.g. `2-1.2.3.4`.
        device: String,
    },
    /// No hub is enumerated at this sysfs location. The bus topology changed
    /// during the search, or a `peer` link named a port whose hub is gone.
    HubMissing {
        /// sysfs location that was looked up, e.g. `2-1.2`.
        location: String,
    },
    /// A hub could not be opened, so whether it switches power is unknown.
    /// Usually a missing udev rule.
    HubUnreadable {
        /// sysfs location of the hub, e.g. `2-1.2`.
        location: String,
    },
    /// The receptacle's other half switches power in ganged mode. VBUS cannot
    /// be cut here: it stays on while either half asks for it (USB 3.2 §10.1,
    /// Table 10-2), and clearing `PORT_POWER` on a ganged hub would take the
    /// other half's neighbouring ports with it.
    PeerNotSwitchable {
        /// The port that would have been cut.
        port: String,
        /// Its peer, which is not individually switchable.
        peer: String,
    },
    /// The kernel named the receptacle's other half through a `peer` link, but
    /// that port holds a device.
    ///
    /// One receptacle holds one device, so the other half must read empty. The
    /// link therefore does not mean what this crate takes it to mean, and
    /// cutting the port would strand whatever is on it.
    PeerNotFound {
        /// The port that would have been cut.
        port: String,
        /// The port the kernel named as its peer.
        candidate: String,
    },
    /// Neither sysfs nor usbfs would switch the port.
    SwitchFailed {
        /// The port that could not be switched.
        port: String,
        /// Why the sysfs `disable` attribute failed, if it was present.
        sysfs: Option<std::io::Error>,
        /// Why the usbfs control transfer failed.
        usbfs: rusb::Error,
    },
    /// Every port feeding the receptacle was switched off, but the device is
    /// still enumerated.
    ///
    /// A powered-off port holds its link in `eSS.Disabled` (USB 3.2 §10.3.1.1),
    /// so a device that stays on the bus means the hub accepted `PORT_POWER`
    /// without acting on it.
    ///
    /// The converse does not hold: a device leaving the bus proves the port was
    /// switched, not that VBUS dropped. With both halves off Table 10-2 only
    /// permits VBUS removal ("May be off"), and a hub that keeps it on for
    /// power applications conforms.
    PowerOffIneffective {
        /// The port that was switched.
        port: String,
    },
    /// The device did not re-enumerate within the timeout.
    NotBack {
        /// The identity that was waited for.
        device: DeviceId,
    },
    /// An enumeration or descriptor read failed.
    Usb(rusb::Error),
}

/// Result alias for this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { device } => write!(f, "no device matching {device} on the bus"),
            Self::NoSwitchableHub { device } => write!(
                f,
                "no hub above {device} does per-port power switching (PPPS)"
            ),
            Self::HubMissing { location } => write!(
                f,
                "no hub enumerated at {location} - did the bus topology change?"
            ),
            Self::HubUnreadable { location } => write!(
                f,
                "hub {location} could not be opened - missing udev rule for usbfs access?"
            ),
            Self::PeerNotSwitchable { port, peer } => write!(
                f,
                "{port} is paired with {peer}, which switches power in ganged mode; \
                 VBUS cannot be cut on this receptacle"
            ),
            Self::PeerNotFound { port, candidate } => write!(
                f,
                "cannot identify the other half of {port}: the kernel names {candidate} \
                 as its peer, but that port is occupied, so it is not the peer"
            ),
            Self::SwitchFailed { port, sysfs, usbfs } => {
                write!(f, "could not switch {port}: usbfs: {usbfs}")?;
                match sysfs {
                    Some(e) => write!(f, "; sysfs: {e}"),
                    None => write!(f, "; sysfs: no `disable` attribute (kernel < 6.0?)"),
                }
            }
            Self::PowerOffIneffective { port } => write!(
                f,
                "{port} was powered off but the device is still enumerated - \
                 the hub accepted PORT_POWER without acting on it \
                 (a powered-off port disables its link, USB 3.2 §10.3.1.1)"
            ),
            Self::NotBack { device } => {
                write!(f, "device {device} did not re-enumerate after power-on")
            }
            Self::Usb(e) => write!(f, "usb error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Usb(e) | Self::SwitchFailed { usbfs: e, .. } => Some(e),
            _ => None,
        }
    }
}

impl From<rusb::Error> for Error {
    fn from(e: rusb::Error) -> Self {
        Self::Usb(e)
    }
}
