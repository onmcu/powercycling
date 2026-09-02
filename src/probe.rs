//! Finding a hub's other half by watching a device's power LED.

use std::io::Write;
use std::time::Duration;

use crate::error::Result;
use crate::hub::{Hub, Hubs, USB_SS};
use crate::pairing::{HubPairs, Pairing, Verdict};
use crate::port::HubPort;
use crate::sysfs::device_location;

/// One cut of the plan: the ports to cut together, the line to declare if
/// the LED goes dark, and why this step ranks where it does.
struct Step {
    ports: Vec<HubPort>,
    line: String,
    why: &'static str,
}

impl Step {
    /// `2-1.4 port 1 + 3-4 port 1`.
    fn ports_text(&self) -> String {
        let names: Vec<String> = self.ports.iter().map(ToString::to_string).collect();
        names.join(" + ")
    }
}

/// What the user typed at a prompt.
enum Answer {
    Yes,
    No,
    Quit,
}

impl Answer {
    fn parse(text: &str, default_yes: bool) -> Self {
        match text.trim().to_ascii_lowercase().as_str() {
            "" => {
                if default_yes {
                    Self::Yes
                } else {
                    Self::No
                }
            }
            "y" | "yes" => Self::Yes,
            "q" | "quit" => Self::Quit,
            _ => Self::No,
        }
    }
}

/// Find which hub shares receptacles with `hub`, by cutting port `port` of it
/// together with each candidate in turn and asking whether the device on it
/// lost power.
///
/// Something with a power LED must be plugged into that port: the device
/// leaves the bus whether its power went or only its link, so only the LED
/// tells.
///
/// The plan is printed first, most likely pair first: candidates are the hubs
/// of the opposite speed that switch power per port, are not paired already,
/// and have port `port` free (or holding a hub, if the probed port does),
/// ranked by same vendor and size as `hub`, then by nearness to the root.
/// Root hubs are candidates: a board may wire the `SuperSpeed` side of its
/// receptacles straight to the host controller. Each step cuts, waits
/// `off_time`, restores, and only then asks - so the ports are never left off
/// while waiting for an answer.
///
/// `ask` shows a prompt and returns what was typed. At a step prompt, Enter
/// runs it, `n` skips it, `q` quits; at the LED prompt, `y` means dark, Enter
/// means lit, `q` quits. Returns the line to declare in the machine's
/// [`HubPairs`] text, or `None` if nothing was found.
///
/// # Errors
///
/// [`crate::Error::HubMissing`] or [`crate::Error::HubUnreadable`] for `hub`,
/// [`crate::Error::SwitchFailed`] if a port would not switch,
/// [`crate::Error::Usb`] if the bus could not be enumerated, or
/// [`crate::Error::Io`] if `ask` or `out` failed.
pub fn probe(
    hub: &str,
    port: u8,
    off_time: Duration,
    pairs: &HubPairs,
    out: &mut impl Write,
    mut ask: impl FnMut(&str) -> std::io::Result<String>,
) -> Result<Option<String>> {
    let hubs = Hubs::enumerate()?;
    let pairing = Pairing::compute(&hubs, pairs);
    if let Verdict::Paired { other, .. } = pairing.verdict(hub) {
        writeln!(
            out,
            "{hub} is already paired with {other} - nothing to probe. To probe it \
             again, drop the declaration that pairs them."
        )?;
        return Ok(None);
    }

    let Some(probed) = probed_port(&hubs, hub, port, out)? else {
        return Ok(None);
    };
    let steps = plan(&hubs, &pairing, &probed);
    print_plan(&steps, &probed, off_time, out)?;

    for (i, step) in steps.iter().enumerate() {
        let prompt = format!(
            "step {}/{}: cut {} [Enter=cut, n=skip, q=quit] ",
            i + 1,
            steps.len(),
            step.ports_text()
        );
        match Answer::parse(&ask(&prompt)?, true) {
            Answer::Quit => return quit(out),
            Answer::No => continue,
            Answer::Yes => {}
        }

        cut_and_restore(&step.ports, off_time)?;

        match Answer::parse(
            &ask("   restored. LED went dark? [y=dark, Enter=lit, q=quit] ")?,
            false,
        ) {
            Answer::Quit => return quit(out),
            Answer::No => continue,
            Answer::Yes => {}
        }

        let found = if i == 0 {
            format!("{probed} cuts VBUS by itself")
        } else {
            format!("{} share a receptacle", step.ports_text())
        };
        writeln!(
            out,
            "\n{found}. Declare this line in your hub pairs:\n\n    {}\n",
            step.line
        )?;
        return Ok(Some(step.line.clone()));
    }

    writeln!(
        out,
        "\nno step cut the power. Either the other half is on a hub that switches \
         power in ganged mode (then this receptacle cannot be cut at all), or its \
         port {port} is occupied, or the hub does not act on PORT_POWER (check with \
         a meter)"
    )?;
    Ok(None)
}

/// Port `port` of the hub at `hub`, if it can be probed: the hub opens,
/// switches power per port, has such a port, and something is plugged into
/// it. Otherwise says why not on `out`.
fn probed_port(hubs: &Hubs, hub: &str, port: u8, out: &mut impl Write) -> Result<Option<HubPort>> {
    let probed_hub = hubs.open_at(hub)?;
    if port > probed_hub.nports {
        writeln!(out, "{hub} has {} ports, no port {port}", probed_hub.nports)?;
        return Ok(None);
    }
    if !probed_hub.per_port_power {
        writeln!(
            out,
            "{hub} switches power in ganged mode, so its ports cannot be cut one \
             at a time and there is nothing to pair it for"
        )?;
        return Ok(None);
    }
    let probed = HubPort::new(probed_hub, port)?;
    if !probed.is_occupied() {
        writeln!(
            out,
            "nothing is plugged into {probed} - plug in a device with a power LED \
             and watch it"
        )?;
        return Ok(None);
    }
    Ok(Some(probed))
}

fn print_plan(
    steps: &[Step],
    probed: &HubPort,
    off_time: Duration,
    out: &mut impl Write,
) -> std::io::Result<()> {
    writeln!(
        out,
        "probing {probed} - watch the power LED of the device on it\n\
         plan, most likely first; each step cuts for {off_time:?}, restores, then asks:"
    )?;
    for (i, step) in steps.iter().enumerate() {
        writeln!(
            out,
            "   {:>2}. {:<40} => \"{}\"  {}",
            i + 1,
            step.ports_text(),
            step.line,
            step.why
        )?;
    }
    if steps.len() == 1 {
        writeln!(
            out,
            "   (no {} hub that switches power per port has port {} free)",
            probed.hub().other_side(),
            probed.port()
        )?;
    }
    writeln!(
        out,
        "Enter runs a step, n skips it, q quits. Ctrl-C during a cut would leave \
         ports off - use q.\n"
    )
}

/// The probed port alone, then with each candidate, most likely first.
fn plan(hubs: &Hubs, pairing: &Pairing, probed: &HubPort) -> Vec<Step> {
    let hub = probed.hub();
    let port = probed.port();

    let mut candidates: Vec<(HubPort, u8, &'static str)> = hubs
        .iter()
        // Cheap filters first, from cached descriptors: not the probed hub,
        // not already settled, opposite speed. Only then open.
        .filter(|dev| {
            let location = device_location(dev);
            location != hub.location
                && matches!(
                    pairing.verdict(&location),
                    Verdict::Unpaired | Verdict::Usb2Hub
                )
        })
        .filter(|dev| {
            dev.device_descriptor()
                .is_ok_and(|d| (d.usb_version() >= USB_SS) != hub.is_super_speed())
        })
        .filter_map(|dev| Hub::open(dev.clone()).ok())
        .filter(|h| h.per_port_power && port <= h.nports)
        .filter_map(|h| HubPort::new(h, port).ok())
        .filter(|p| p.is_holdable_for(probed))
        .map(|p| {
            let (rank, why) = likelihood(hub, p.hub());
            (p, rank, why)
        })
        .collect();
    // The other half of a chip is the same chip: same vendor, same size. Of
    // those, the one nearest the root - a board that splices a hub into one
    // side only puts the other side's hub higher up, never lower.
    candidates.sort_by_key(|(p, rank, _)| (*rank, p.hub().depth(), p.hub().location.clone()));

    let mut steps = vec![Step {
        ports: vec![probed.clone()],
        line: format!("{} none", hub.location),
        why: "",
    }];
    steps.extend(candidates.into_iter().map(|(candidate, _, why)| {
        let line = if hub.is_super_speed() {
            format!("{} {}", candidate.hub().location, hub.location)
        } else {
            format!("{} {}", hub.location, candidate.hub().location)
        };
        Step {
            ports: vec![candidate, probed.clone()],
            line,
            why,
        }
    }));
    steps
}

/// How likely `candidate` is the other half of `hub`, lower first.
const fn likelihood(hub: &Hub, candidate: &Hub) -> (u8, &'static str) {
    match (candidate.vid == hub.vid, candidate.nports == hub.nports) {
        (true, true) => (0, "same vendor and size"),
        (true, false) => (1, "same vendor"),
        (false, true) => (2, "same size"),
        (false, false) => (3, ""),
    }
}

fn quit(out: &mut impl Write) -> Result<Option<String>> {
    writeln!(out, "quit - every port is restored")?;
    Ok(None)
}

/// Cut `ports` in order, wait `off_time`, restore them in reverse order.
///
/// Restores whatever the cut did, reporting the first failure of either.
fn cut_and_restore(ports: &[HubPort], off_time: Duration) -> Result<()> {
    let cut = ports.iter().try_for_each(|p| p.set_power(false));
    if cut.is_ok() {
        std::thread::sleep(off_time);
    }
    let restored = ports
        .iter()
        .rev()
        .map(|p| p.set_power(true))
        .fold(Ok(()), Result::and);
    cut.and(restored)
}
