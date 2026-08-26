//! Why a power cycle could not be carried out.

use crate::device::DeviceId;

/// Why a power cycle could not be carried out.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Nothing on the bus matches this `vid:pid:serial`.
    #[error("no device matching {device} on the bus")]
    NotFound {
        /// The identity that was searched for.
        device: DeviceId,
    },
    /// More than one device matches this `vid:pid:serial`, so it is unknown
    /// which one to cut. Narrow the identity with a serial.
    #[error("{count} devices match {device} - add a serial to pick one")]
    Ambiguous {
        /// The identity that was searched for.
        device: DeviceId,
        /// How many devices carry it.
        count: usize,
    },
    /// No hub between the device and the root hub does per-port power
    /// switching, so nothing can cut its VBUS.
    ///
    /// Root hubs are not considered: they are host controller ports, which the
    /// hub chapter of the specification does not cover (USB 3.2 §10.1, "Host
    /// controller ports may have different requirements").
    #[error("no hub above {device} does per-port power switching (PPPS)")]
    NoSwitchableHub {
        /// sysfs location of the device, e.g. `2-1.2.3.4`.
        device: String,
    },
    /// No hub is enumerated at this sysfs location. The bus topology changed
    /// during the search, or a `peer` link named a port whose hub is gone.
    #[error("no hub enumerated at {location} - did the bus topology change?")]
    HubMissing {
        /// sysfs location that was looked up, e.g. `2-1.2`.
        location: String,
    },
    /// A hub could not be opened or would not answer, so whether it switches
    /// power is unknown. Access denied usually means a missing udev rule.
    #[error("hub {location} could not be read: {source}{}", udev_hint(*.source))]
    HubUnreadable {
        /// sysfs location of the hub, e.g. `2-1.2`.
        location: String,
        /// What failed: opening the hub, or reading its descriptors.
        #[source]
        source: rusb::Error,
    },
    /// The receptacle's other half switches power in ganged mode. VBUS cannot
    /// be cut here: it stays on while either half asks for it (USB 3.2 §10.1,
    /// Table 10-2), and clearing `PORT_POWER` on a ganged hub would take the
    /// other half's neighbouring ports with it.
    #[error(
        "{port} is paired with {peer}, which switches power in ganged mode; \
         VBUS cannot be cut on this receptacle"
    )]
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
    #[error(
        "cannot identify the other half of {port}: the kernel names {candidate} \
         as its peer, but that port is occupied, so it is not the peer"
    )]
    PeerNotFound {
        /// The port that would have been cut.
        port: String,
        /// The port the kernel named as its peer.
        candidate: String,
    },
    /// Neither sysfs nor usbfs would switch the port.
    #[error(
        "could not switch {port}: usbfs: {usbfs}; sysfs: {}",
        .sysfs.as_ref().map_or_else(
            || "no `disable` attribute (kernel < 6.0?)".to_string(),
            std::string::ToString::to_string,
        )
    )]
    SwitchFailed {
        /// The port that could not be switched.
        port: String,
        /// Why the sysfs `disable` attribute failed, if it was present.
        sysfs: Option<std::io::Error>,
        /// Why the usbfs control transfer failed.
        #[source]
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
    #[error(
        "{port} was powered off but the device is still enumerated - \
         the hub accepted PORT_POWER without acting on it \
         (a powered-off port disables its link, USB 3.2 §10.3.1.1)"
    )]
    PowerOffIneffective {
        /// The port that was switched.
        port: String,
    },
    /// The device did not re-enumerate within the timeout.
    #[error("device {device} did not re-enumerate after power-on")]
    NotBack {
        /// The identity that was waited for.
        device: DeviceId,
    },
    /// An enumeration or descriptor read failed.
    #[error("usb error: {0}")]
    Usb(#[from] rusb::Error),
}

/// Result alias for this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// The likely fix when a hub refused access, appended to the message.
const fn udev_hint(source: rusb::Error) -> &'static str {
    match source {
        rusb::Error::Access => " - missing udev rule for usbfs access?",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    fn switch_failed(sysfs: Option<std::io::Error>) -> Error {
        Error::SwitchFailed {
            port: "2-1.2 port 3".to_string(),
            sysfs,
            usbfs: rusb::Error::Access,
        }
    }

    fn hub_unreadable(source: rusb::Error) -> Error {
        Error::HubUnreadable {
            location: "2-1.2".to_string(),
            source,
        }
    }

    #[test]
    fn switch_failed_names_both_routes() {
        let missing = switch_failed(None).to_string();
        assert!(missing.ends_with("sysfs: no `disable` attribute (kernel < 6.0?)"));

        let denied = switch_failed(Some(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        )))
        .to_string();
        assert!(denied.contains("usbfs: "));
        assert!(denied.contains("sysfs: permission denied"));
    }

    #[test]
    fn hub_unreadable_hints_at_udev_only_when_access_was_denied() {
        let denied = hub_unreadable(rusb::Error::Access).to_string();
        assert!(denied.starts_with("hub 2-1.2 could not be read: "));
        assert!(denied.ends_with("missing udev rule for usbfs access?"));

        let timeout = hub_unreadable(rusb::Error::Timeout).to_string();
        assert!(timeout.contains("Operation timed out"));
        assert!(!timeout.contains("udev"));
    }

    #[test]
    fn usb_errors_are_the_source() {
        assert!(switch_failed(None).source().is_some());
        assert!(hub_unreadable(rusb::Error::Io).source().is_some());
        assert!(Error::from(rusb::Error::NoDevice).source().is_some());
        assert!(
            Error::PowerOffIneffective {
                port: "2-1.2 port 3".to_string()
            }
            .source()
            .is_none()
        );
    }
}
