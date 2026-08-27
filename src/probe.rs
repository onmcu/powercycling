//! Finding a hub's other half by watching a device's power LED.

use std::io::Write;
use std::time::Duration;

use crate::error::Result;
use crate::hub::{Hub, Hubs};
use crate::port::HubPort;

/// Find which hub shares receptacles with `hub`, by cutting port `port` of it
/// together with each candidate in turn and asking whether the device on it
/// lost power.
///
/// Something with a power LED must be plugged into that port. Nothing about a
/// VBUS drop is visible over USB - the device leaves the bus whether its power
/// went or only its link - so the observer is the only instrument. Every cut
/// is announced through `confirm` first and restored before the next question.
///
/// Candidates are the hubs of the opposite speed that switch power per port
/// and whose port `port` is empty (or holds a hub, if the probed port does:
/// the two halves of one hub sit on one receptacle). Root hubs are included:
/// a board may wire the `SuperSpeed` side of its receptacles straight to the
/// host controller.
///
/// Returns the line to declare in the machine's [`crate::HubPairs`] text, or
/// `None` if no candidate cut the power. `confirm` is given a question and returns the
/// answer; `Err` from it aborts the probe with the ports restored.
///
/// # Errors
///
/// [`crate::Error::HubMissing`] or [`crate::Error::HubUnreadable`] for `hub`,
/// [`crate::Error::SwitchFailed`] if a port would not switch, or
/// [`crate::Error::Usb`] if the bus could not be enumerated, or
/// [`crate::Error::Io`] if `confirm` or `out` failed.
pub fn probe(
    hub: &str,
    port: u8,
    off_time: Duration,
    out: &mut impl Write,
    mut confirm: impl FnMut(&str) -> std::io::Result<bool>,
) -> Result<Option<String>> {
    let hubs = Hubs::enumerate()?;
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
    let target_is_hub = probed.holds_hub();

    // Alone first: if this half gates VBUS by itself, there is no pair to find.
    writeln!(out, "step 1: {probed} alone")?;
    if cut_and_ask(&[&probed], off_time, out, &mut confirm)? {
        let line = format!("{hub} none");
        writeln!(
            out,
            "\n{probed} cuts VBUS by itself. Declare this line in your hub pairs:\n\n    {line}\n"
        )?;
        return Ok(Some(line));
    }

    let candidates: Vec<HubPort> = hubs
        .iter()
        .filter_map(|dev| Hub::open(dev.clone()).ok())
        .filter(|h| {
            h.is_super_speed() != probed.is_super_speed() && h.per_port_power && port <= h.nports
        })
        .filter_map(|h| HubPort::new(h, port).ok())
        .filter(|p| p.is_holdable(target_is_hub))
        .collect();
    if candidates.is_empty() {
        writeln!(
            out,
            "no {} hub that switches power per port has an empty port {port} - \
             nothing to try",
            other_side(&probed)
        )?;
        return Ok(None);
    }

    writeln!(
        out,
        "step 2: {probed} together with each of {} candidate(s)",
        candidates.len()
    )?;
    for candidate in &candidates {
        if cut_and_ask(&[candidate, &probed], off_time, out, &mut confirm)? {
            let line = if probed.is_super_speed() {
                format!("{} {hub}", candidate.hub().location)
            } else {
                format!("{hub} {}", candidate.hub().location)
            };
            writeln!(
                out,
                "\n{probed} and {candidate} share a receptacle. Declare this line in \
                 your hub pairs:\n\n    {line}\n"
            )?;
            return Ok(Some(line));
        }
    }

    writeln!(
        out,
        "\nno candidate cut the power. Either the other half is on a hub that \
         switches power in ganged mode (then this receptacle cannot be cut at \
         all), or its port {port} is occupied, or the hub does not act on \
         PORT_POWER (check with a meter)"
    )?;
    Ok(None)
}

fn other_side(port: &HubPort) -> &'static str {
    if port.is_super_speed() {
        "USB 2.0"
    } else {
        "SuperSpeed"
    }
}

/// Cut `ports` in order for `off_time`, ask whether the LED went dark, and
/// restore them in reverse order whatever happened.
fn cut_and_ask(
    ports: &[&HubPort],
    off_time: Duration,
    out: &mut impl Write,
    confirm: &mut impl FnMut(&str) -> std::io::Result<bool>,
) -> Result<bool> {
    let names: Vec<String> = ports.iter().map(ToString::to_string).collect();
    if !confirm(&format!(
        "   cut {} for {off_time:?} and watch the LED? [y/N] ",
        names.join(" + ")
    ))? {
        writeln!(out, "   skipped")?;
        return Ok(false);
    }

    let cut = ports.iter().try_for_each(|p| p.set_power(false));
    let asked = if cut.is_ok() {
        std::thread::sleep(off_time);
        confirm("   did the power LED go dark? [y/N] ")
    } else {
        Ok(false)
    };
    // Restore every port regardless, reporting the first failure.
    let restored = ports
        .iter()
        .rev()
        .map(|p| p.set_power(true))
        .fold(Ok(()), Result::and);
    cut.and(restored)?;
    Ok(asked?)
}
