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
//! 3. Descent: a hub on port N of a paired hub pairs with the hub on port N
//!    of its partner, when the two are of opposite speeds, the same vendor,
//!    and the same size.
//!
//! Rule 3 is what the kernel does to fill its `peer` links, minus the sanity
//! checks, which is why those links are not consulted: on a board that
//! splices a USB 2.0-only hub into the USB 2.0 path alone, the kernel's rule
//! pairs the wrong hubs, and every hub below inherits the mistake. The board
//! itself is the only source for such a pairing, hence rule 1.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::str::FromStr;

use rusb::Version;

use crate::device::Device;
use crate::error::{Error, Result};
use crate::hub::{Hub, Hubs, USB_BOS, USB_SS};
use crate::sysfs::{child_location, controller_of, device_location, sysfs_location};

/// The word that declares a hub to have no other half.
const NONE: &str = "none";

/// Hub pairs declared for a machine, for what the bus cannot tell.
///
/// Needed only where a board wires the two halves of its receptacles to hubs
/// that share no ancestry - typically a USB 2.0-only hub on the USB 2.0 side
/// and the host controller's own ports on the `SuperSpeed` side. One pair per
/// such board oddity; everything below the pair is derived.
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
/// # Raspberry Pi CM5 IO board: the USB 2.0 side goes through an on-board
/// # hub, the SuperSpeed side straight to the controller.
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

/// How two hubs were found to be one package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Evidence {
    /// Declared in the pairs file.
    Declared,
    /// Root hubs of the same host controller.
    Controller,
    /// On the same port number of hubs already paired.
    Descent(u8),
}

impl std::fmt::Display for Evidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Declared => write!(f, "declared"),
            Self::Controller => write!(f, "same host controller"),
            Self::Descent(port) => write!(f, "port {port} of paired hubs"),
        }
    }
}

/// What pairing needs to know about one hub.
struct Node {
    location: String,
    bus: u8,
    path: Vec<u8>,
    version: Version,
    vid: u16,
    pid: u16,
    /// From the hub descriptor, if the hub could be opened.
    opened: Option<(u8, bool)>,
}

impl Node {
    fn read(dev: &Device) -> Option<Self> {
        let desc = dev.device_descriptor().ok()?;
        Some(Self {
            location: device_location(dev),
            bus: dev.bus_number(),
            path: dev.port_numbers().ok()?,
            version: desc.usb_version(),
            vid: desc.vendor_id(),
            pid: desc.product_id(),
            opened: Hub::open(dev.clone())
                .ok()
                .map(|hub| (hub.nports, hub.per_port_power)),
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
            && match (self.opened, other.opened) {
                (Some((n, _)), Some((m, _))) => n == m,
                _ => true,
            }
    }
}

/// Every hub on the bus and which is the other half of which.
pub struct Pairing {
    nodes: Vec<Node>,
    by_location: HashMap<String, usize>,
    pairs: HashMap<String, (String, Evidence)>,
    alone: HashSet<String>,
    /// Declared hubs that are not on the bus.
    declared_absent: Vec<String>,
}

impl Pairing {
    /// Pair every hub that can be paired.
    ///
    /// Opens every hub for its descriptor. One that cannot be opened is still
    /// paired if the rules allow, just without the port-count check.
    pub fn compute(hubs: &Hubs, declared: &HubPairs) -> Self {
        let mut nodes: Vec<Node> = hubs.iter().filter_map(Node::read).collect();
        // Parents before children, so rule 3 sees a hub's parent already paired.
        nodes.sort_by_key(|n| n.path.len());
        let by_location = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.location.clone(), i))
            .collect();
        let mut pairing = Self {
            nodes,
            by_location,
            pairs: HashMap::new(),
            alone: HashSet::new(),
            declared_absent: Vec::new(),
        };
        pairing.apply_declared(declared);
        pairing.pair_root_hubs();
        pairing.pair_by_descent();
        pairing
    }

    fn link(&mut self, a: &str, b: &str, evidence: Evidence) {
        self.pairs.insert(a.to_string(), (b.to_string(), evidence));
        self.pairs.insert(b.to_string(), (a.to_string(), evidence));
    }

    fn is_settled(&self, location: &str) -> bool {
        self.pairs.contains_key(location) || self.alone.contains(location)
    }

    fn apply_declared(&mut self, declared: &HubPairs) {
        for (a, b) in &declared.pairs {
            for hub in [a, b] {
                if !self.by_location.contains_key(hub) {
                    self.declared_absent.push(hub.clone());
                }
            }
            self.link(a, b, Evidence::Declared);
        }
        for hub in &declared.alone {
            if !self.by_location.contains_key(hub) {
                self.declared_absent.push(hub.clone());
            }
            self.alone.insert(hub.clone());
        }
    }

    /// Rule 2: the two root hubs of one host controller.
    fn pair_root_hubs(&mut self) {
        let mut by_controller: HashMap<std::path::PathBuf, Vec<usize>> = HashMap::new();
        for (i, node) in self.nodes.iter().enumerate() {
            if node.is_root()
                && !self.is_settled(&node.location)
                && let Some(controller) = controller_of(&node.location)
            {
                by_controller.entry(controller).or_default().push(i);
            }
        }
        for roots in by_controller.values() {
            if let [a, b] = roots[..] {
                let (a, b) = (&self.nodes[a], &self.nodes[b]);
                if a.is_super_speed() != b.is_super_speed() {
                    let (a, b) = (a.location.clone(), b.location.clone());
                    self.link(&a, &b, Evidence::Controller);
                }
            }
        }
    }

    /// Rule 3: same port number of hubs already paired.
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
            let Some((partner, _)) = self.pairs.get(&parent) else {
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
            self.link(&location, &candidate, Evidence::Descent(port));
        }
    }

    /// The other half of the hub at `location`, if known.
    pub fn other_half(&self, location: &str) -> Option<&str> {
        self.pairs.get(location).map(|(other, _)| other.as_str())
    }

    /// Whether the hub at `location` is known to have no other half: declared
    /// so, or a USB 2.0 hub.
    pub fn is_alone(&self, location: &str) -> bool {
        self.alone.contains(location)
            || self
                .by_location
                .get(location)
                .is_some_and(|&i| !self.nodes[i].may_have_other_half())
    }

    /// Write every hub with its pairing and the evidence for it to `out`,
    /// followed by what to do about any hub left unpaired.
    ///
    /// # Errors
    ///
    /// Only if `out` could not be written to.
    pub fn report(&self, out: &mut impl Write) -> std::io::Result<()> {
        let mut order: Vec<&Node> = self.nodes.iter().collect();
        order.sort_by(|a, b| (a.bus, &a.path).cmp(&(b.bus, &b.path)));

        let mut unpaired = Vec::new();
        for node in order {
            let ports = node.opened.map_or_else(
                || "unreadable       ".to_string(),
                |(n, ppps)| format!("{n:>2} ports  {}", if ppps { "ppps  " } else { "ganged" }),
            );
            let verdict = if let Some((other, evidence)) = self.pairs.get(&node.location) {
                format!("<-> {other:<10} {evidence}")
            } else if self.alone.contains(&node.location) {
                "no other half (declared)".to_string()
            } else if !node.may_have_other_half() {
                "no other half (USB 2.0 hub)".to_string()
            } else {
                unpaired.push(node);
                "UNPAIRED".to_string()
            };
            writeln!(
                out,
                "   {:<10} USB {}.{}  {:04x}:{:04x}  {ports}  {verdict}",
                node.location,
                node.version.major(),
                node.version.minor(),
                node.vid,
                node.pid,
            )?;
        }

        for hub in &self.declared_absent {
            writeln!(
                out,
                "   note: {hub} is declared as a pair but not on the bus"
            )?;
        }

        if !unpaired.is_empty() {
            writeln!(out)?;
            for node in &unpaired {
                writeln!(
                    out,
                    "   {} declares USB {}.{}, so it is one half of a USB 3.x hub, but \
                     the other half could not be identified.",
                    node.location,
                    node.version.major(),
                    node.version.minor()
                )?;
            }
            writeln!(
                out,
                "   Both halves of a receptacle keep its VBUS on, so a device on such a \
                 hub cannot be power-cycled until\n   \
                 the pair is known. The bus cannot tell; probe the hub with a device \
                 that has a power LED and declare\n   \
                 the pair it finds in your hub pairs."
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
