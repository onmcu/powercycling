//! Power-cycle one device from the command line.
//!
//!     cargo run --example cycle -- 0483 374e 0050003A3233511639363634
//!     cargo run --example cycle -- 0483 374e 0050003A3233511639363634 --debug
//!     cargo run --example cycle -- 0483 374e 0050003A3233511639363634 --verify
//!     cargo run --example cycle -- 0483 374e 0050003A3233511639363634 --primary-only

use powercycling::{Error, HubPort, PowerPorts};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

const OFF_TIME: Duration = Duration::from_secs(2);

fn main() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [vid, pid, rest @ ..] = args.as_slice() else {
        eprintln!("usage: cycle <vid-hex> <pid-hex> [serial] [--debug|--verify|--primary-only]");
        std::process::exit(2);
    };
    let (Ok(vid), Ok(pid)) = (u16::from_str_radix(vid, 16), u16::from_str_radix(pid, 16)) else {
        eprintln!("vid and pid must be hex, e.g. 0483 374e");
        std::process::exit(2);
    };

    let serial = if rest.first().is_some_and(|serial| !serial.starts_with("--")) {
        rest.first().cloned()
    } else {
        None
    };

    let has = |flag: &str| rest.iter().any(|a| a == flag);

    if has("--debug") {
        return powercycling::debug_scan(vid, pid, serial.as_deref());
    }
    if has("--verify") {
        return verify(vid, pid, serial.as_deref());
    }
    if has("--primary-only") {
        return primary_only(vid, pid, serial.as_deref());
    }

    let t0 = Instant::now();
    let ports = PowerPorts::find(vid, pid, serial.as_deref())?;
    let found = t0.elapsed();
    describe(&ports);

    ports.cycle(OFF_TIME)?;
    let cycled = t0.elapsed();
    let dev = powercycling::wait_for_device(vid, pid, serial.as_deref(), Duration::from_secs(10))?;
    println!("back at {}", location(&dev));
    println!(
        "find {:?}, cycle {:?} (off {:?}), re-enumerate {:?}, total {:?}",
        found,
        cycled.saturating_sub(found),
        OFF_TIME,
        t0.elapsed().saturating_sub(cycled),
        t0.elapsed()
    );
    Ok(())
}

fn describe(ports: &PowerPorts) {
    println!("cutting {:?}", ports.primary);
    match ports.held() {
        [] => println!("   nothing held down (USB 2.0 only receptacle)"),
        [one] => println!("   holding down {one:?} (kernel `peer` link)"),
        many => println!(
            "   holding down {} empty opposite-speed ports: {}",
            many.len(),
            many.iter()
                .map(HubPort::location)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// e.g. `2-1.2.3.3`.
fn location(dev: &powercycling::Device) -> String {
    let path: Vec<String> = dev
        .port_numbers()
        .unwrap_or_default()
        .iter()
        .map(u8::to_string)
        .collect();
    format!("{}-{}", dev.bus_number(), path.join("."))
}

/// Cut *only* the device's own port, deliberately skipping the other half.
///
/// If the LED goes dark anyway, this hub gates VBUS on the USB 2.0 port alone
/// and the whole hold-down mechanism is unnecessary for it. Watch the board,
/// not the bus: it drops off USB either way.
fn primary_only(vid: u16, pid: u16, serial: Option<&str>) -> Result<(), Error> {
    let ports = PowerPorts::find(vid, pid, serial)?;
    println!(
        "cutting ONLY {:?}, leaving {} port(s) powered",
        ports.primary,
        ports.held().len()
    );
    println!("watch the board's power LED for {OFF_TIME:?}...");

    ports.primary.set_power(false)?;
    std::thread::sleep(OFF_TIME);
    ports.primary.set_power(true)?;

    powercycling::wait_for_device(vid, pid, serial, Duration::from_secs(10))?;
    println!("done - LED dark => VBUS is gated on this port alone");
    println!("       LED lit  => the other half kept it alive, hold-down is needed");
    Ok(())
}

/// Every USB *device* currently enumerated, by sysfs location. Interface
/// directories (`2-1:1.0`) are skipped; root hubs (`usb2`) are kept.
fn bus_snapshot() -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir("/sys/bus/usb/devices") else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.contains(':'))
        .collect()
}

/// Cut power, look at what actually left the bus, and report anything that
/// dropped which should not have.
///
/// This measures USB enumeration, not VBUS. A device that stays enumerated
/// definitely kept its power, which is exactly what "did I disturb my other
/// boards?" asks. Proving the *target* lost VBUS rather than just its link
/// still needs an LED or a meter.
fn verify(vid: u16, pid: u16, serial: Option<&str>) -> Result<(), Error> {
    let t = Instant::now();
    let ports = PowerPorts::find(vid, pid, serial)?;
    println!("found ports in {:?}", t.elapsed());
    describe(&ports);

    // Ports we hold down are empty, so nothing should vanish on their account.
    let expected = ports.primary.child_location();
    println!("expecting only {expected} (and anything below) to drop\n");

    let before = bus_snapshot();
    let t0 = Instant::now();
    ports.set_power(false)?;
    let cut = t0.elapsed();
    // Let the kernel notice the disconnect before sampling.
    std::thread::sleep(Duration::from_millis(750));
    let during = bus_snapshot();
    let t1 = Instant::now();
    ports.set_power(true)?;
    let restore = t1.elapsed();
    powercycling::wait_for_device(vid, pid, serial, Duration::from_secs(10))?;
    std::thread::sleep(Duration::from_millis(750));
    let after = bus_snapshot();

    let intended = |d: &String| *d == expected || d.starts_with(&format!("{expected}."));

    let mut collateral = Vec::new();
    println!("left the bus while powered off:");
    for d in before.difference(&during) {
        if intended(d) {
            println!("   {d:<18} intended");
        } else {
            println!("   {d:<18} <== COLLATERAL");
            collateral.push(d.clone());
        }
    }

    let missing: Vec<&String> = before.difference(&after).collect();
    if !missing.is_empty() {
        println!("\ndid not come back:");
        for d in &missing {
            println!("   {d}");
        }
    }

    println!();
    if !before.difference(&during).any(intended) {
        println!("WARNING: the target never left the bus - power was not cut at all");
    } else if collateral.is_empty() && missing.is_empty() {
        println!("OK: only the intended port's subtree dropped, everything came back");
    }
    if !collateral.is_empty() {
        println!(
            "PROBLEM: {} unrelated device(s) lost their connection",
            collateral.len()
        );
    }
    println!(
        "\ncut took {cut:?}, restore took {restore:?} ({} ports held down)",
        ports.held().len()
    );
    Ok(())
}
