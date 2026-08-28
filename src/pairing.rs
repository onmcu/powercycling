//! Pairing the two logical hubs of a USB 3.x hub.
//!
//! A USB 3.x hub is a USB 2.0 hub and a `SuperSpeed` hub in one package
//! (USB 3.2 §10.1), enumerated as two devices on two buses. Once the two are
//! known to be one package, their ports pair by number: "the port numbers
//! assigned to a specific port by the hub shall be consistent between the USB
//! 2.0 hub and Enhanced `SuperSpeed` hub" (§10.3.3). So the receptacle is
//! identified by pairing hubs, and never by guessing at ports.
//!
//! Hubs are paired by, in order of trust:
//!
//! 1. The machine's [`HubPairs`] - what the user declared.
//! 2. Sharing a host controller: the two root hubs of one xHCI controller.
//! 3. Expansion: a controller whose two root hubs differ in port count, the
//!    smaller having exactly one port that holds a hub with as many ports as
//!    the larger root, and no twin of its own on the bus. Such a board runs
//!    one side of its receptacles through that hub, so the hub stands in for
//!    the smaller root: its port N and the larger root's port N are one
//!    receptacle.
//! 4. Descent: a hub on port N of a paired hub pairs with the hub on port N
//!    of its peer, if the two are of opposite speeds, the same vendor and
//!    the same size. The check only rejects; a hub that fails it stays
//!    unpaired.
//!
//! The kernel's `peer` links are rule 4 without the check. On a board of the
//! rule 3 kind they pair the wrong hubs, and every hub below inherits the
//! mistake, so they are not used.
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
/// the pairing rules do not recognize. Everything below a pair is derived.
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HubPairs {
    pairs: Vec<(String, String)>,
    alone: Vec<String>,
}

impl HubPairs {
    /// No declared pairs.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            pairs: Vec::new(),
            alone: Vec::new(),
        }
    }

    /// Read pairs from a file in the text format.
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

    /// Declare that port N of `a` and port N of `b` share a receptacle.
    #[must_use]
    pub fn pair(mut self, a: &str, b: &str) -> Self {
        self.pairs.push((a.to_string(), b.to_string()));
        self
    }

    /// Declare that `hub`'s receptacles have no other half.
    #[must_use]
    pub fn alone(mut self, hub: &str) -> Self {
        self.alone.push(hub.to_string());
        self
    }
}

impl FromStr for HubPairs {
    type Err = Error;

    /// Parse the text format.
    ///
    /// # Errors
    ///
    /// [`Error::PairsSyntax`] naming the first line that is not a pair.
    fn from_str(text: &str) -> Result<Self> {
        let mut pairs = Self::none();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                continue;
            }
            let syntax = || Error::PairsSyntax {
                line: i + 1,
                text: raw.to_string(),
            };
            let mut words = line.split_whitespace();
            let (Some(a), Some(b), None) = (words.next(), words.next(), words.next()) else {
                return Err(syntax());
            };
            match (a, b) {
                (NONE, NONE) => return Err(syntax()),
                (NONE, hub) | (hub, NONE) => pairs.alone.push(hub.to_string()),
                (a, b) if a == b => return Err(syntax()),
                (a, b) => pairs.pairs.push((a.to_string(), b.to_string())),
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
    by_location: HashMap<String, usize>,
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
        let by_location = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.location.clone(), i))
            .collect();
        let mut pairing = Self {
            nodes,
            by_location,
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

    fn is_settled(&self, location: &str) -> bool {
        self.status.contains_key(location)
    }

    fn apply_declared(&mut self, declared: &HubPairs) {
        for (a, b) in &declared.pairs {
            for hub in [a, b] {
                if !self.by_location.contains_key(hub) {
                    self.declared_absent.push(hub.clone());
                }
            }
            self.link(a, b, &Evidence::Declared);
        }
        for hub in &declared.alone {
            if !self.by_location.contains_key(hub) {
                self.declared_absent.push(hub.clone());
            }
            self.status.insert(hub.clone(), Status::DeclaredAlone);
        }
    }

    /// Rule 2: the two root hubs of one host controller.
    fn pair_root_hubs(&mut self) {
        let mut by_controller: HashMap<PathBuf, Vec<usize>> = HashMap::new();
        for (i, node) in self.nodes.iter().enumerate() {
            if !self.is_settled(&node.location)
                && let Some(controller) = &node.controller
            {
                by_controller.entry(controller.clone()).or_default().push(i);
            }
        }
        let links: Vec<(String, String)> = by_controller
            .values()
            .filter_map(|roots| {
                let [a, b] = roots[..] else { return None };
                let (a, b) = (&self.nodes[a], &self.nodes[b]);
                (a.is_super_speed() != b.is_super_speed())
                    .then(|| (a.location.clone(), b.location.clone()))
            })
            .collect();
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
        let controller_pairs: Vec<(usize, usize)> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, node)| {
                let Status::Paired {
                    other,
                    evidence: Evidence::Controller,
                } = self.status.get(&node.location)?
                else {
                    return None;
                };
                let p = *self.by_location.get(other)?;
                (i < p).then_some((i, p))
            })
            .collect();

        for (a, b) in controller_pairs {
            let (a, b) = (&self.nodes[a], &self.nodes[b]);
            let (small, large, ports) = match (a.nports, b.nports) {
                (Some(1), Some(n)) if n > 1 => (a, b, n),
                (Some(n), Some(1)) if n > 1 => (b, a, n),
                _ => continue,
            };
            let hub_location = child_location(&small.location, small.bus, 1);
            let Some(&h) = self.by_location.get(&hub_location) else {
                continue;
            };
            let hub = &self.nodes[h];
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
        for i in 0..self.nodes.len() {
            let node = &self.nodes[i];
            if node.is_root() || self.is_settled(&node.location) || !node.may_have_other_half() {
                continue;
            }
            let Some((&port, parent_path)) = node.path.split_last() else {
                continue;
            };
            let parent = sysfs_location(node.bus, parent_path);
            let Some(partner) = self.other_half(&parent) else {
                continue;
            };
            let Some(&p) = self.by_location.get(partner) else {
                continue;
            };
            let candidate = child_location(partner, self.nodes[p].bus, port);
            let Some(&c) = self.by_location.get(&candidate) else {
                continue;
            };
            if self.is_settled(&candidate) || !node.matches(&self.nodes[c]) {
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
            None => match self.by_location.get(location) {
                Some(&i) if self.nodes[i].may_have_other_half() => Verdict::Unpaired,
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
