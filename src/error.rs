//! Why a power cycle could not be carried out.

/// Why a power cycle could not be carried out.
#[derive(Debug)]
pub enum Error {
    /// Nothing on the bus matches this `vid:pid:serial`.
    NotFound {
        /// Vendor ID that was searched for.
        vid: u16,
        /// Product ID that was searched for.
        pid: u16,
        /// Serial that was searched for.
        serial: Option<String>,
    },
    /// No hub between the device and the root controller does per-port power
    /// switching, so nothing can cut its VBUS.
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
    /// The receptacle's other half, named by a kernel `peer` link, switches
    /// power in ganged mode. VBUS cannot be cut here.
    PeerNotSwitchable {
        /// The port that would have been cut.
        port: String,
        /// Its peer, which is not individually switchable.
        peer: String,
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
    /// Power was switched off, but the device is still enumerated: the hub
    /// reports per-port power switching without performing it.
    PowerOffIneffective {
        /// The port that was switched.
        port: String,
    },
    /// The device did not re-enumerate within the timeout.
    NotBack {
        /// Vendor ID that was waited for.
        vid: u16,
        /// Product ID that was waited for.
        pid: u16,
        /// Serial that was waited for.
        serial: Option<String>,
    },
    /// An enumeration or descriptor read failed.
    Usb(rusb::Error),
}

/// Result alias for this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { vid, pid, serial } => {
                write!(f, "no device {vid:04x}:{pid:04x} with serial {serial:?}")
            }
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
                 the hub reports per-port power switching but does not do it"
            ),
            Self::NotBack { vid, pid, serial } => write!(
                f,
                "device {vid:04x}:{pid:04x} {serial:?} did not re-enumerate after power-on"
            ),
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
