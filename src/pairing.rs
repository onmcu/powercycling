//! Pairing the two logical hubs of a USB 3.x hub. The rules are documented
//! on [`HubPairs`]; this module applies them in that order, one method each.
//!
//! Pairing reads cached descriptors and sysfs only; no hub is opened.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use rusb::Version;

use crate::device::Device;
use crate::error::{Error, Result};
use crate::hub::{Hubs, USB_BOS, USB_SS};
use crate::sysfs::{
    child_location, controller_of, device_location, read_max_child, sysfs_location,
};

/// The word that declares a hub to have no other half.
const NONE: &str = "none";

/// Hub pairs declared for a machine, for what the bus cannot tell.
///
/// Needed only where a board wires the two halves of its receptacles in a way
/// the [rules](#how-hubs-are-paired) do not recognize. Everything below a
/// pair is derived.
///
/// The pairs are the caller's to supply: built with [`Self::pair`] and
/// [`Self::alone`], parsed from text with [`str::parse`], or loaded from a
/// file with [`Self::load`]. The crate reads no file on its own.
///
/// # Text format
///
/// One pair per line, as two sysfs hub locations in either order, or one
/// location and `none` for a hub whose receptacles have no other half.
/// `#` starts a comment.
///
/// ```text
/// # The USB 2.0 side of the receptacles goes through an on-board hub, the
/// # SuperSpeed side straight to the controller.
/// 2-1 usb3
/// ```
///
/// # Why hubs are paired
///
/// A USB 3.x receptacle is one socket carrying two USB links: a USB 2.0 hub
/// and a `SuperSpeed` hub each own a port on it. A device uses one of them and
/// leaves the other empty; a USB 3.x _hub_ occupies both. The socket has one
/// VBUS pin, and it is on while either port is powered:
///
/// > If either the USB 2.0 hub or Enhanced `SuperSpeed` hub controllers
/// > requires a downstream port to be powered, power is turned on for the
/// > port. (USB 3.2 §10.1; normative in Table 10-2)
///
/// Cutting only the half the device sits on drops it off the bus, not off
/// power: the debug session dies, the MCU keeps running. Both halves have to
/// be down at once.
///
/// Port _numbers_ are known: "the port numbers assigned to a specific port by
/// the hub shall be consistent between the USB 2.0 hub and Enhanced
/// `SuperSpeed` hub" (§10.3.3). So the question is which _hub_ is the other
/// half of this one.
///
/// # How hubs are paired
///
/// In order of confidence:
///
/// 1. **Declared:** what this `HubPairs` says.
/// 2. **Same host controller:** the two root hubs of one xHCI controller are
///    the two halves of its receptacles. True on every machine seen so far;
///    the specification does not guarantee it.
/// 3. **Expansion:** some boards route one side of their receptacles through
///    a hub. The tell: one root of the controller has a _single_ port, and on
///    it hangs a hub with exactly as many ports as the other root (and no
///    twin of its own on the bus):
///
///    ```text
///    usb2 (1 port) ── 2-1 (4 ports) ─ port 1 ─ port 2 ─ port 3 ─ port 4
///                                        │        │        │        │     the same four receptacles
///    usb3 (4 ports) ───────────────── port 1 ─ port 2 ─ port 3 ─ port 4
///    ```
///
///    Every receptacle needs a port on each side, and the only USB 2.0 ports
///    there are belong to that hub. So it stands in for the small root:
///    `2-1 <-> usb3`, port N to port N. (The same with the sides swapped.)
/// 4. **Descent:** paired hubs have paired ports (§10.3.3), so the hub on
///    port N of one is the other half of the hub on port N of its peer.
///    Walking down from a paired hub-pair, this finds every hub below it,
///    three identical ones or not. Before pairing, the two are checked for
///    what the halves of one chip must share: opposite speeds, the same
///    vendor, the same number of ports. The check finds nothing on its own;
///    it only rejects. A hub that fails it stays unpaired
///    ([`Error::HubUnpaired`]) rather than being paired with a wrong hub.
///
/// Once a hub is paired, so is everything chained below it, and the held-down
/// port is port N of the peer. [`crate::tree`] shows it: switching mode,
/// other half and how it was found, and what sits on every port. A Raspberry
/// Pi CM5 IO board with one Realtek hub in receptacle 4 and two more chained
/// below it:
///
/// ```text
/// usb2         1d6b:0002  "xHCI Host Controller"  USB 2.00  1 ports  ppps    same host controller as usb3; 2-1 stands in for it
/// └─ port 1: 2-1          2109:3431  "USB2.0 Hub"  USB 2.10  4 ports  ganged  <-> usb3 (expands usb2's single port to 4)
///    ├─ port 1: -
///    ├─ port 2: -
///    ├─ port 3: -
///    └─ port 4: 2-1.4        0bda:5411  "USB2.1 Hub"  USB 2.10  4 ports  ppps    <-> 3-4 (port 4 of paired parents)
///       ├─ port 1: 2-1.4.1      1a40:0101  "USB 2.0 Hub"  USB 2.00  4 ports  ganged  no other half (USB 2.0 hub)
///       │  ├─ port 1: 2-1.4.1.1    0483:374e  "STLINK-V3"  serial 0050003A3233511639363634
///       │  ├─ port 2: -
///       │  ├─ port 3: -
///       │  └─ port 4: -
///       ├─ port 2: -
///       ├─ port 3: 2-1.4.3      0bda:5411  "USB2.1 Hub"  USB 2.10  4 ports  ppps    <-> 3-4.3 (port 3 of paired parents)
///       │  └─ …
///       └─ port 4: 2-1.4.4      0bda:5411  "USB2.1 Hub"  USB 2.10  4 ports  ppps    <-> 3-4.4 (port 4 of paired parents)
///          └─ …
/// usb3         1d6b:0003  "xHCI Host Controller"  USB 3.00  4 ports  ppps    <-> 2-1 (expands usb2's single port to 4)
/// ├─ port 1: -
/// ├─ port 2: -
/// ├─ port 3: -
/// └─ port 4: 3-4          0bda:0411  "USB3.2 Hub"  USB 3.20  4 ports  ppps    <-> 2-1.4 (port 4 of paired parents)
///    ├─ port 1: -
///    ├─ port 2: -
///    ├─ port 3: 3-4.3        0bda:0411  "USB3.2 Hub"  USB 3.20  4 ports  ppps    <-> 2-1.4.3 (port 3 of paired parents)
///    │  └─ …
///    └─ port 4: 3-4.4        0bda:0411  "USB3.2 Hub"  USB 3.20  4 ports  ppps    <-> 2-1.4.4 (port 4 of paired parents)
///       └─ …
/// ```
///
/// **How to read it:** the USB 2.0 side of the controller (`usb2`) has a
/// single port. It feeds a 4-port hub whose own `SuperSpeed` half is nowhere
/// on the bus. So the board runs the USB 2.0 lines of its four receptacles
/// through that hub, but the `SuperSpeed` lines go straight to the host
/// controller's (`usb3`) four ports (rule 3). Below that, every hub pairs
/// with its twin by port number (rule 4), three identical hubs or not. The
/// STLINK on `2-1.4.1.1` sits on a plain USB 2.0 hub that is ganged, so
/// cycling it alone is refused ([`Error::BehindHub`]);
/// [`PowerPorts::find_above`](crate::PowerPorts::find_above) with one level
/// cycles that hub (the carrier) through `2-1.4 port 1` and `3-4 port 1`
/// together.
///
/// # What the rules cannot cover
///
/// What rule 4's check guards against is a hub spliced into one side only,
/// without rule 3's single-port tell:
///
/// ```text
/// usb2 (4 ports) ─ port 2 ─ 2-2 (4 ports) ─ port 1 ─ port 2 ─ port 3 ─ port 4
///                                               │        │        │        │    the same four receptacles
/// usb3 (4 ports) ───────────────────────── port 1 ─ port 2 ─ port 3 ─ port 4
/// ```
///
/// On the bus, `2-2` looks like an ordinary hub in receptacle 2, and rule 4
/// would pair it with whatever hub sits on `usb3` port 2. The check rejects
/// that unless it is a look-alike hub. This board needs a declaration:
/// `2-2 usb3`. [`crate::probe`] finds it by watching a power LED.
///
/// The kernel's own `peer` links are _not_ used. They are rule 4 without the
/// check, and on the board of rule 3 they pair the wrong hubs:
///
/// ```text
/// $ readlink usb3/3-0:1.0/usb3-port1/peer
/// ../../../usb2/2-0:1.0/usb2-port1
/// ```
///
/// This says `usb3 <-> usb2`; in practice it is `usb3 <-> 2-1`. Every hub
/// below would inherit the mistake.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HubPairs {
    pairs: Vec<(String, String)>,
    alone: Vec<String>,
}

impl HubPairs {
    /// Construct from a Vec of `pairs` and a Vec of hubs that are `alone`
    #[must_use]
    pub const fn from(pairs: Vec<(String, String)>, alone: Vec<String>) -> Self {
        Self { pairs, alone }
    }

    /// No declared pairs.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            pairs: Vec::new(),
            alone: Vec::new(),
        }
    }

    /// Read pairs from a file in the [text format](Self#text-format).
    ///
    /// # Errors
    ///
    /// [`Error::PairsUnreadable`] if the file could not be read,
    /// [`Error::PairsSyntax`] if a line is not a pair.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        std::fs::read_to_string(path)
            .map_err(|source| Error::PairsUnreadable {
                path: path.to_path_buf(),
                source,
            })?
            .parse()
    }

    /// Add a mapping: port N of `a` and port N of `b` share a receptacle.
    #[must_use]
    pub fn pair(mut self, a: &str, b: &str) -> Self {
        self.pairs.push((a.to_string(), b.to_string()));
        self
    }

    /// Add an empty mapping: `hub`'s receptacles have **no** other half.
    #[must_use]
    pub fn alone(mut self, hub: &str) -> Self {
        self.alone.push(hub.to_string());
        self
    }
}

impl FromStr for HubPairs {
    type Err = Error;

    /// Parse the [text format](Self#text-format).
    ///
    /// # Errors
    ///
    /// [`Error::PairsSyntax`] naming the first line that is not a pair.
    fn from_str(text: &str) -> Result<Self> {
        let mut pairs = Self::none();
        for (i, raw) in text.lines().enumerate() {
            // Drop the comment, i.e., anything after the first `#`
            let line = raw.split_once('#').map_or(raw, |(before, _)| before);
            // Blank lines and comment-only lines declare nothing.
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Errors quote the line as written, comment included, numbered
            // from 1 as editors do.
            let syntax = || Error::PairsSyntax {
                line: i + 1,
                text: raw.to_string(),
            };
            // Exactly two words: `a b`, `a none` or `none a`.
            let mut words = line.split_whitespace();
            let (Some(a), Some(b), None) = (words.next(), words.next(), words.next()) else {
                return Err(syntax());
            };
            match (a, b) {
                // `none none` names no hub, `a a` pairs a hub with itself.
                (NONE, NONE) => return Err(syntax()),
                (NONE, hub) | (hub, NONE) => pairs = pairs.alone(hub),
                (a, b) if a == b => return Err(syntax()),
                (a, b) => pairs = pairs.pair(a, b),
            }
        }
        Ok(pairs)
    }
}

/// How two hubs were found to share receptacles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence {
    /// Declared in the [`HubPairs`].
    Declared,
    /// Root hubs of the same host controller.
    Controller,
    /// A hub standing in for the single-port root hub `root`, expanding it
    /// to the `ports` ports of the controller's other root hub.
    Expansion {
        /// The single-port root hub the hub sits on.
        root: String,
        /// How many ports both the hub and the other root hub have.
        ports: u8,
    },
    /// On the same port number of hubs already paired.
    Descent(u8),
}

impl std::fmt::Display for Evidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Declared => write!(f, "declared"),
            Self::Controller => write!(f, "same host controller"),
            Self::Expansion { root, ports } => {
                write!(f, "expands {root}'s single port to {ports}")
            }
            Self::Descent(port) => write!(f, "port {port} of paired parents"),
        }
    }
}

/// What is known about a hub's other half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict<'a> {
    /// Paired with the hub at this location.
    Paired {
        /// sysfs location of the other half.
        other: &'a str,
        /// How the pair was found.
        evidence: &'a Evidence,
    },
    /// A single-port root hub whose controller sibling `other` is paired with
    /// the hub `via` on that port instead - the hub stands in for this one.
    StoodInFor {
        /// The controller sibling.
        other: &'a str,
        /// The hub on this root's single port.
        via: &'a str,
    },
    /// Declared to have no other half.
    DeclaredAlone,
    /// A USB 2.0 hub, which has no `SuperSpeed` half.
    Usb2Hub,
    /// Has another half that could not be identified.
    Unpaired,
    /// Not a hub this pairing knows of.
    Unknown,
}

impl std::fmt::Display for Verdict<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Paired { other, evidence } => write!(f, "<-> {other} ({evidence})"),
            Self::StoodInFor { other, via } => {
                write!(f, "same host controller as {other}; {via} stands in for it")
            }
            Self::DeclaredAlone => write!(f, "no other half (declared)"),
            Self::Usb2Hub => write!(f, "no other half (USB 2.0 hub)"),
            Self::Unpaired => write!(f, "UNPAIRED - other half unknown"),
            Self::Unknown => write!(f, "unreadable"),
        }
    }
}

/// What the rules settled for one hub. The owned form of [`Verdict`]'s
/// settled variants.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Paired { other: String, evidence: Evidence },
    StoodInFor { other: String, via: String },
    DeclaredAlone,
}

/// What pairing needs to know about one hub.
struct Node {
    /// sysfs location of a device, e.g. `2-1.2.1.1`, or `usb2` for a root hub.
    location: String,
    bus: u8,
    path: Vec<u8>,
    version: Version,
    vid: u16,
    /// `bNbrPorts`, from sysfs, if readable.
    nports: Option<u8>,
    /// The host controller, for a root hub.
    controller: Option<PathBuf>,
}

impl Node {
    fn read(dev: &Device) -> Option<Self> {
        let desc = dev.device_descriptor().ok()?;
        let location = device_location(dev);
        let path = dev.port_numbers().ok()?;
        Some(Self {
            controller: path.is_empty().then(|| controller_of(&location)).flatten(),
            nports: read_max_child(&location),
            location,
            bus: dev.bus_number(),
            path,
            version: desc.usb_version(),
            vid: desc.vendor_id(),
        })
    }

    const fn is_root(&self) -> bool {
        self.path.is_empty()
    }

    const fn is_super_speed(&self) -> bool {
        self.version.0 >= USB_SS.0
    }

    /// Whether this hub may be one half of a USB 3.x hub, i.e. have another
    /// half to look for. A USB 2.0 hub has none.
    fn may_have_other_half(&self) -> bool {
        self.version >= USB_BOS
    }

    /// Whether `other` could be the other half of this hub: opposite speed,
    /// same vendor, same number of ports where both are known.
    const fn matches(&self, other: &Self) -> bool {
        self.is_super_speed() != other.is_super_speed()
            && self.vid == other.vid
            && match (self.nports, other.nports) {
                (Some(n), Some(m)) => n == m,
                _ => true,
            }
    }
}

/// Every hub on the bus and which is the other half of which.
pub struct Pairing {
    nodes: Vec<Node>,
    status: HashMap<String, Status>,
    /// Declared hubs that are not on the bus.
    declared_absent: Vec<String>,
}

impl Pairing {
    /// Pair every hub that can be paired. Reads cached descriptors and sysfs
    /// only.
    pub fn compute(hubs: &Hubs, declared: &HubPairs) -> Self {
        Self::from_nodes(hubs.iter().filter_map(Node::read).collect(), declared)
    }

    fn from_nodes(mut nodes: Vec<Node>, declared: &HubPairs) -> Self {
        // Parents before children, so rule 4 sees a hub's parent already paired.
        nodes.sort_by_key(|n| n.path.len());
        let mut pairing = Self {
            nodes,
            status: HashMap::new(),
            declared_absent: Vec::new(),
        };
        pairing.apply_declared(declared);
        pairing.pair_root_hubs();
        pairing.pair_by_expansion();
        pairing.pair_by_descent();
        pairing
    }

    fn link(&mut self, a: &str, b: &str, evidence: &Evidence) {
        for (x, y) in [(a, b), (b, a)] {
            self.status.insert(
                x.to_string(),
                Status::Paired {
                    other: y.to_string(),
                    evidence: evidence.clone(),
                },
            );
        }
    }

    /// The hub at `location`, if it is on the bus.
    fn node(&self, location: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.location == location)
    }

    fn is_settled(&self, location: &str) -> bool {
        self.status.contains_key(location)
    }

    fn apply_declared(&mut self, declared: &HubPairs) {
        for (a, b) in &declared.pairs {
            for hub in [a, b] {
                if self.node(hub).is_none() {
                    self.declared_absent.push(hub.clone());
                }
            }
            self.link(a, b, &Evidence::Declared);
        }
        for hub in &declared.alone {
            if self.node(hub).is_none() {
                self.declared_absent.push(hub.clone());
            }
            self.status.insert(hub.clone(), Status::DeclaredAlone);
        }
    }

    /// Rule 2: the two root hubs of one host controller.
    fn pair_root_hubs(&mut self) {
        // Group the root hubs (the only nodes with a controller) by the host
        // controller they belong to, skipping those a declaration already settled.
        let mut by_controller: HashMap<&Path, Vec<&Node>> = HashMap::new();
        for node in &self.nodes {
            if !self.is_settled(&node.location)
                && let Some(controller) = &node.controller
            {
                by_controller.entry(controller).or_default().push(node);
            }
        }
        // A controller with exactly two roots of opposite speed - one USB 2.0,
        // one SuperSpeed - has a pair. Anything else is left alone.
        let links: Vec<(String, String)> = by_controller
            .values()
            .filter_map(|roots| {
                let [a, b] = roots[..] else { return None };
                (a.is_super_speed() != b.is_super_speed())
                    .then(|| (a.location.clone(), b.location.clone()))
            })
            .collect();
        // Record the pairs. Done after the scan: `link` mutates `status`,
        // which the scan above reads.
        for (a, b) in links {
            self.link(&a, &b, &Evidence::Controller);
        }
    }

    /// Rule 3: a hub standing in for the single-port root hub of a lopsided
    /// controller.
    ///
    /// A receptacle needs a port on each side, so when one root hub has one
    /// port and the other has several, that side of the receptacles is
    /// reached through whatever the single port holds. If that is a hub with
    /// exactly as many ports as the other root, it is the board's way of
    /// fanning that side out, and its ports line up with the other root's.
    fn pair_by_expansion(&mut self) {
        // The root hub pairs (as locations) found by rule 2; the loop below
        // mutates `status`, so the scan cannot hold references.
        let controller_pairs: Vec<(String, String)> = self
            .nodes
            .iter()
            .filter_map(|node| {
                let Status::Paired {
                    other,
                    evidence: Evidence::Controller,
                } = self.status.get(&node.location)?
                else {
                    return None;
                };
                // Each pair once, from the half whose location sorts first.
                (node.location < *other).then(|| (node.location.clone(), other.clone()))
            })
            .collect();

        for (a, b) in controller_pairs {
            let (Some(a), Some(b)) = (self.node(&a), self.node(&b)) else {
                continue;
            };
            // The tell: one root with a single port, the other with several.
            let (small, large, ports) = match (a.nports, b.nports) {
                (Some(1), Some(n)) if n > 1 => (a, b, n),
                (Some(n), Some(1)) if n > 1 => (b, a, n),
                _ => continue,
            };
            // Whatever hangs on that single port is the stand-in candidate.
            let hub_location = child_location(&small.location, small.bus, 1);
            let Some(hub) = self.node(&hub_location) else {
                continue;
            };
            // It must fan the small side out to the large root: not spoken
            // for already, as many ports as the large root, on the opposite
            // side of the receptacles (opposite speed).
            if self.is_settled(&hub_location)
                || hub.nports != Some(ports)
                || hub.is_super_speed() == large.is_super_speed()
                // A hub whose own other half is on the bus is not a stand-in.
                || self.nodes.iter().any(|n| {
                    n.location != hub.location
                        && !self.is_settled(&n.location)
                        && hub.matches(n)
                })
            {
                continue;
            }
            // Pair the stand-in with the large root, and mark the small root
            // as spoken for: it pairs with nothing itself.
            let evidence = Evidence::Expansion {
                root: small.location.clone(),
                ports,
            };
            let (small, large) = (small.location.clone(), large.location.clone());
            self.link(&hub_location, &large, &evidence);
            self.status.insert(
                small,
                Status::StoodInFor {
                    other: large,
                    via: hub_location,
                },
            );
        }
    }

    /// Rule 4: same port number of hubs already paired.
    fn pair_by_descent(&mut self) {
        // Indexed, not iterated: `link` at the end needs `&mut self`. Nodes
        // are sorted parents first, so a parent's pair is settled before its
        // children are looked at.
        for i in 0..self.nodes.len() {
            let node = &self.nodes[i];
            // Only unsettled USB 3.x hubs below a root have a half to find.
            if node.is_root() || self.is_settled(&node.location) || !node.may_have_other_half() {
                continue;
            }
            // Split the location into the parent hub and the port on it.
            let Some((&port, parent_path)) = node.path.split_last() else {
                continue;
            };
            let parent = sysfs_location(node.bus, parent_path);
            // The parent's other half; without one, nothing to descend from.
            let Some(partner) = self.other_half(&parent) else {
                continue;
            };
            // The same port number on the partner is where the other half
            // must sit (§10.3.3).
            let Some(partner_bus) = self.node(partner).map(|p| p.bus) else {
                continue;
            };
            let candidate = child_location(partner, partner_bus, port);
            let Some(candidate_node) = self.node(&candidate) else {
                continue;
            };
            // Reject rather than mis-pair: the candidate must be free and
            // look like the other half of the same chip.
            if self.is_settled(&candidate) || !node.matches(candidate_node) {
                continue;
            }
            let location = node.location.clone();
            self.link(&location, &candidate, &Evidence::Descent(port));
        }
    }

    /// The other half of the hub at `location`, if known.
    pub fn other_half(&self, location: &str) -> Option<&str> {
        match self.status.get(location) {
            Some(Status::Paired { other, .. }) => Some(other),
            _ => None,
        }
    }

    /// What is known about the other half of the hub at `location`.
    pub fn verdict(&self, location: &str) -> Verdict<'_> {
        match self.status.get(location) {
            Some(Status::Paired { other, evidence }) => Verdict::Paired { other, evidence },
            Some(Status::StoodInFor { other, via }) => Verdict::StoodInFor { other, via },
            Some(Status::DeclaredAlone) => Verdict::DeclaredAlone,
            None => match self.node(location) {
                Some(node) if node.may_have_other_half() => Verdict::Unpaired,
                Some(_) => Verdict::Usb2Hub,
                None => Verdict::Unknown,
            },
        }
    }

    /// The hubs with another half that could not be identified.
    pub fn unpaired(&self) -> Vec<&str> {
        self.nodes
            .iter()
            .filter(|n| self.verdict(&n.location) == Verdict::Unpaired)
            .map(|n| n.location.as_str())
            .collect()
    }

    /// Declared hubs that are not on the bus.
    pub fn declared_absent(&self) -> &[String] {
        &self.declared_absent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hub on the bus, for building topologies without hardware.
    fn hub(location: &str, version: (u8, u8), vid: u16, nports: u8) -> Node {
        let (bus, path) = location.strip_prefix("usb").map_or_else(
            || {
                let (bus, path) = location.split_once('-').unwrap();
                (
                    bus.parse().unwrap(),
                    path.split('.').map(|p| p.parse().unwrap()).collect(),
                )
            },
            |bus| (bus.parse().unwrap(), Vec::new()),
        );
        Node {
            controller: path
                .is_empty()
                .then(|| PathBuf::from(format!("controller-of-bus-{}", bus / 2))),
            location: location.to_string(),
            bus,
            path,
            version: Version(version.0, version.1, 0),
            vid,
            nports: Some(nports),
        }
    }

    const LINUX: u16 = 0x1d6b;
    const VIA: u16 = 0x2109;
    const REALTEK: u16 = 0x0bda;

    /// Raspberry Pi CM5 IO board: the controller's USB 2.0 root has one
    /// port, holding a 4-port VIA hub; its `SuperSpeed` root has four ports.
    /// Three identical Realtek hubs hang below, one carrying two more.
    fn cm5() -> Vec<Node> {
        vec![
            hub("usb2", (2, 0), LINUX, 1),
            hub("usb3", (3, 0), LINUX, 4),
            hub("2-1", (2, 1), VIA, 4),
            hub("2-1.4", (2, 1), REALTEK, 4),
            hub("2-1.4.3", (2, 1), REALTEK, 4),
            hub("2-1.4.4", (2, 1), REALTEK, 4),
            hub("3-4", (3, 2), REALTEK, 4),
            hub("3-4.3", (3, 2), REALTEK, 4),
            hub("3-4.4", (3, 2), REALTEK, 4),
        ]
    }

    #[test]
    fn expansion_pairs_the_stand_in_hub_with_the_larger_root() {
        let pairing = Pairing::from_nodes(cm5(), &HubPairs::none());
        assert_eq!(pairing.other_half("2-1"), Some("usb3"));
        assert_eq!(pairing.other_half("usb3"), Some("2-1"));
        assert_eq!(
            pairing.verdict("2-1"),
            Verdict::Paired {
                other: "usb3",
                evidence: &Evidence::Expansion {
                    root: "usb2".to_string(),
                    ports: 4,
                },
            }
        );
        // The small root is spoken for, but pairs with nothing itself.
        assert_eq!(
            pairing.verdict("usb2"),
            Verdict::StoodInFor {
                other: "usb3",
                via: "2-1",
            }
        );
        assert_eq!(pairing.other_half("usb2"), None);
        // Everything below follows by descent, identical hubs included.
        assert_eq!(pairing.other_half("2-1.4"), Some("3-4"));
        assert_eq!(pairing.other_half("3-4.3"), Some("2-1.4.3"));
        assert_eq!(pairing.other_half("2-1.4.4"), Some("3-4.4"));
        assert!(pairing.unpaired().is_empty());
    }

    #[test]
    fn a_declared_pair_wins_over_expansion() {
        let declared = HubPairs::none().pair("2-1", "usb3");
        let pairing = Pairing::from_nodes(cm5(), &declared);
        assert_eq!(
            pairing.verdict("2-1"),
            Verdict::Paired {
                other: "usb3",
                evidence: &Evidence::Declared,
            }
        );
        assert_eq!(pairing.other_half("2-1.4"), Some("3-4"));
    }

    #[test]
    fn symmetric_pc_pairs_by_descent_only() {
        // Both roots have four ports; a Realtek hub in receptacle 1 shows up
        // on both. Nothing to expand.
        let pairing = Pairing::from_nodes(
            vec![
                hub("usb2", (2, 0), LINUX, 4),
                hub("usb3", (3, 0), LINUX, 4),
                hub("2-1", (2, 1), REALTEK, 4),
                hub("3-1", (3, 2), REALTEK, 4),
            ],
            &HubPairs::none(),
        );
        assert_eq!(pairing.other_half("usb2"), Some("usb3"));
        assert_eq!(
            pairing.verdict("2-1"),
            Verdict::Paired {
                other: "3-1",
                evidence: &Evidence::Descent(1),
            }
        );
    }

    #[test]
    fn expansion_needs_matching_port_counts_and_no_twin() {
        // Single-port root, but the hub on it has 7 ports, not the root's 4.
        let pairing = Pairing::from_nodes(
            vec![
                hub("usb2", (2, 0), LINUX, 1),
                hub("usb3", (3, 0), LINUX, 4),
                hub("2-1", (2, 1), VIA, 7),
            ],
            &HubPairs::none(),
        );
        assert_eq!(pairing.other_half("2-1"), None);
        assert_eq!(pairing.unpaired(), vec!["2-1"]);

        // Counts match, but the hub's own SuperSpeed twin is on the bus, so
        // it is a real USB 3.x hub in a real receptacle, not a stand-in.
        let pairing = Pairing::from_nodes(
            vec![
                hub("usb2", (2, 0), LINUX, 1),
                hub("usb3", (3, 0), LINUX, 4),
                hub("2-1", (2, 1), VIA, 4),
                hub("3-2", (3, 2), VIA, 4),
            ],
            &HubPairs::none(),
        );
        assert_eq!(pairing.other_half("2-1"), None);
    }

    #[test]
    fn a_plain_usb2_hub_has_no_other_half() {
        let pairing = Pairing::from_nodes(
            vec![
                hub("usb2", (2, 0), LINUX, 4),
                hub("usb3", (3, 0), LINUX, 4),
                hub("2-1", (2, 0), 0x1a40, 4),
            ],
            &HubPairs::none(),
        );
        assert_eq!(pairing.verdict("2-1"), Verdict::Usb2Hub);
        assert!(pairing.unpaired().is_empty());
    }

    #[test]
    fn declared_alone_and_unknown_hubs() {
        let pairing = Pairing::from_nodes(
            vec![hub("2-1", (2, 1), VIA, 4)],
            &HubPairs::none().alone("2-1").alone("9-9"),
        );
        assert_eq!(pairing.verdict("2-1"), Verdict::DeclaredAlone);
        // A declaration stands even for a hub that is not on the bus - the
        // report just notes that - and nothing is known about the undeclared.
        assert_eq!(pairing.verdict("9-9"), Verdict::DeclaredAlone);
        assert_eq!(pairing.declared_absent(), ["9-9".to_string()]);
        assert_eq!(pairing.verdict("8-8"), Verdict::Unknown);
    }

    #[test]
    fn parses_pairs_comments_and_none() {
        let text = "# CM5\n2-1 usb3  # trailing\n\nnone 2-4\n2-3 none\n";
        let pairs: HubPairs = text.parse().unwrap();
        assert_eq!(
            pairs,
            HubPairs::none()
                .pair("2-1", "usb3")
                .alone("2-4")
                .alone("2-3")
        );
    }

    #[test]
    fn rejects_malformed_lines() {
        for bad in ["2-1", "2-1 usb3 extra", "none none", "2-1 2-1"] {
            let err = format!("# ok\n{bad}\n").parse::<HubPairs>().unwrap_err();
            match err {
                Error::PairsSyntax { line, text } => {
                    assert_eq!(line, 2);
                    assert_eq!(text, bad);
                }
                other => panic!("unexpected {other}"),
            }
        }
    }

    #[test]
    fn empty_file_declares_nothing() {
        assert_eq!("".parse::<HubPairs>().unwrap(), HubPairs::none());
        assert_eq!(
            "# only comments\n".parse::<HubPairs>().unwrap(),
            HubPairs::none()
        );
    }
}
