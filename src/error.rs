//! Why a power cycle could not be carried out.

use std::path::PathBuf;

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
    /// The device's own hub does not switch power per port, and the nearest
    /// port above that does feeds that hub whole. Cutting it would take every
    /// device on the hub, so it is refused rather than done on the caller's
    /// behalf.
    ///
    /// To cycle the hub and everything on it deliberately, use
    /// [`crate::PowerPorts::find_above`] with `levels`.
    #[error(
        "{device} is behind hub {hub}{}, which does not switch power per port; \
         cutting the port above it would take every device on that hub. To cycle \
         that hub and everything on it deliberately, ask for the hub {levels} \
         level(s) above the device (find_above)",
        hub_id_hint(.hub_id.as_ref())
    )]
    BehindHub {
        /// sysfs location of the device, e.g. `2-1.2.3.4`.
        device: String,
        /// sysfs location of the hub between it and the switchable port, e.g.
        /// `2-1.2.3`.
        hub: String,
        /// That hub's identity, if its descriptor could be read.
        hub_id: Option<DeviceId>,
        /// How many hubs above the device it is - what
        /// [`crate::PowerPorts::find_above`] takes to cycle it.
        levels: u8,
    },
    /// [`crate::PowerPorts::find_above`] was asked for more hubs above the
    /// device than there are below the root hub. Root hub ports are host
    /// controller ports and are not switched.
    #[error("{device} has fewer than {levels} switchable hub(s) above it")]
    NothingAbove {
        /// sysfs location of the device, e.g. `2-1.2.3.4`.
        device: String,
        /// How many levels up were asked for.
        levels: u8,
    },
    /// The hub at the requested level above the device does not have the
    /// requested `DeviceId`.
    #[error("hub {hub}{} above {device} is not the expected {expected}",
        hub_id_hint(.found.as_ref()))]
    HubMismatch {
        /// sysfs location of the device, e.g. `2-1.2.3.4`.
        device: String,
        /// sysfs location of the hub that was found, e.g. `2-1.2`.
        hub: String,
        /// That hub's identity, if its descriptor could be read.
        found: Option<DeviceId>,
        /// The identity that was required.
        expected: DeviceId,
    },
    /// No hub is enumerated at this sysfs location. The bus topology changed
    /// during the search, or a declared pair names a hub that is not there.
    #[error(
        "no hub enumerated at {location} - did the bus topology change, or does a \
         declared pair name a hub that is not there?"
    )]
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
    /// The other half of the receptacle holds a device of its own, which one
    /// receptacle cannot (unless the device is a hub, whose two halves sit one
    /// on each port, USB 3.2 §10.1). The pairing is wrong, and cutting the
    /// port would strand whatever is on it.
    #[error(
        "the other half of {port}'s receptacle should be {candidate}, but that port \
         holds a device of its own, so the hub pairing is wrong - check the USB tree"
    )]
    PeerNotFound {
        /// The port that would have been cut.
        port: String,
        /// The port the pairing named as its other half.
        candidate: String,
    },
    /// The port is on one half of a USB 3.x hub whose other half could not be
    /// identified. VBUS stays on while either half powers the receptacle, so
    /// cutting this port alone would only disconnect the device; nothing is
    /// cut.
    ///
    /// The pair has to be declared once for the machine, see
    /// [`crate::HubPairs`]. [`crate::tree`] shows every hub and its pairing;
    /// [`crate::probe`] finds the pair by watching a device's power LED.
    #[error(
        "{port} is on one half of a USB 3.x hub whose {other_side} half could not \
         be identified; cutting it alone would only disconnect the device, not \
         power it off. Declare the pair for this machine: the USB tree shows the \
         hubs, probing {hub} with a device that has a power LED finds it"
    )]
    HubUnpaired {
        /// The port that would have been cut.
        port: String,
        /// sysfs location of its hub, e.g. `2-1.2`.
        hub: String,
        /// Which side is missing: `USB 2.0` or `SuperSpeed`.
        other_side: &'static str,
    },
    /// A line of the hub pairs text is not a pair.
    #[error(
        "line {line} of the hub pairs is not a pair: `{text}` \
         (expected `<hub> <hub>` or `<hub> none`, e.g. `2-1 usb3`)"
    )]
    PairsSyntax {
        /// 1-based line number.
        line: usize,
        /// The line as written.
        text: String,
    },
    /// The pairs file could not be read.
    #[error("hub pairs file {} could not be read: {source}", .path.display())]
    PairsUnreadable {
        /// The file.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
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
    /// still enumerated: a powered-off port disables its link (USB 3.2
    /// §10.3.1.1), so the hub accepted `PORT_POWER` without acting on it.
    ///
    /// The converse does not hold: a device leaving the bus proves the port
    /// was switched, not that VBUS dropped (Table 10-2: "May be off").
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
    /// Writing a report, or reading an answer, failed.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result alias for this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// The hub's identity in parentheses, where it could be read.
fn hub_id_hint(id: Option<&DeviceId>) -> String {
    id.map_or_else(String::new, |id| format!(" ({id})"))
}

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
    fn behind_hub_names_the_hub_to_target() {
        let with_id = Error::BehindHub {
            device: "2-1.2.3.4".to_string(),
            hub: "2-1.2.3".to_string(),
            hub_id: Some(DeviceId::new(0x0bda, 0x5411, None)),
            levels: 1,
        }
        .to_string();
        assert!(with_id.starts_with("2-1.2.3.4 is behind hub 2-1.2.3 (0bda:5411), "));
        assert!(with_id.ends_with("ask for the hub 1 level(s) above the device (find_above)"));

        let without = Error::BehindHub {
            device: "2-1.2.3.4".to_string(),
            hub: "2-1.2.3".to_string(),
            hub_id: None,
            levels: 1,
        }
        .to_string();
        assert!(without.starts_with("2-1.2.3.4 is behind hub 2-1.2.3, "));
    }

    #[test]
    fn hub_mismatch_names_both_identities() {
        let mismatch = Error::HubMismatch {
            device: "2-1.2.3".to_string(),
            hub: "2-1.2".to_string(),
            found: Some(DeviceId::new(0x0bda, 0x5411, None)),
            expected: DeviceId::new(0x0424, 0x2514, None),
        };
        assert_eq!(
            mismatch.to_string(),
            "hub 2-1.2 (0bda:5411) above 2-1.2.3 is not the expected 0424:2514"
        );
    }

    #[test]
    fn hub_unpaired_says_what_to_do() {
        let msg = Error::HubUnpaired {
            port: "2-1.2 port 4".to_string(),
            hub: "2-1.2".to_string(),
            other_side: "SuperSpeed",
        }
        .to_string();
        assert!(
            msg.starts_with("2-1.2 port 4 is on one half of a USB 3.x hub whose SuperSpeed half")
        );
        assert!(msg.contains("probing 2-1.2 with a device that has a power LED"));
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
