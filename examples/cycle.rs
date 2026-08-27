//! Power-cycle one device from the command line.
//!
//!     cargo run --example cycle -- 0483 374e 0050003A3233511639363634
//!     cargo run --example cycle -- 0483 374e 0050003A3233511639363634 --debug
//!     cargo run --example cycle -- 0483 374e 0050003A3233511639363634 --verify
//!     cargo run --example cycle -- 0483 374e 0050003A3233511639363634 --primary-only
//!     cargo run --example cycle -- --pairs
//!     cargo run --example cycle -- --probe 2-1.2 4
//!
//! Every form takes `--pairs-file <path>` for the hub pairs the machine needs
//! declared (see `HubPairs`); without it, none are.

use powercycling::{DeviceId, Error, HubPairs, HubPort, PowerPorts};
use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

const OFF_TIME: Duration = Duration::from_secs(2);

fn main() {
    // `fn main() -> Result` would print the error with `Debug`; the messages
    // are written for `Display` using the "thiserror" crate.
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // `--pairs-file <path>` anywhere in the arguments; none declared without it.
    let pairs = match args.iter().position(|a| a == "--pairs-file") {
        Some(i) if i + 1 < args.len() => {
            let path = args.remove(i + 1);
            args.remove(i);
            HubPairs::load(path)?
        }
        Some(_) => {
            eprintln!("--pairs-file needs a path");
            std::process::exit(2);
        }
        None => HubPairs::none(),
    };

    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["--pairs"] => {
            return Ok(powercycling::pairing_report(
                &pairs,
                &mut std::io::stdout(),
            )?);
        }
        ["--probe", hub, port] => return probe(hub, port.parse()?),
        _ => {}
    }
    let [vid, pid, rest @ ..] = args.as_slice() else {
        eprintln!(
            "usage: cycle <vid-hex> <pid-hex> [serial] [--debug|--verify|--primary-only]\n       \
             cycle --pairs\n       \
             cycle --probe <hub> <port>\n       \
             each with an optional --pairs-file <path>"
        );
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
    let device = DeviceId::new(vid, pid, serial.as_deref());

    if has("--debug") {
        return Ok(powercycling::debug_scan(
            &device,
            &pairs,
            &mut std::io::stdout(),
        )?);
    }
    if has("--verify") {
        return Ok(verify(&device, &pairs)?);
    }
    if has("--primary-only") {
        return Ok(primary_only(&device, &pairs)?);
    }

    let t0 = Instant::now();
    let ports = find(&device, &pairs)?;
    let found = t0.elapsed();
    describe(&ports);

    ports.cycle(OFF_TIME)?;
    let cycled = t0.elapsed();
    let dev = powercycling::wait_for_device(&device, Duration::from_secs(10))?;
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

/// `PowerPorts::find`, with the command-line way out spelled out for the one
/// error whose fix is a declaration rather than a change of target.
fn find(device: &DeviceId, pairs: &HubPairs) -> powercycling::Result<PowerPorts> {
    PowerPorts::find(device, pairs).inspect_err(|e| {
        if let Error::HubUnpaired { hub, .. } = e {
            eprintln!(
                "hint: `cycle --pairs` shows the hub pairing, `cycle --probe {hub} <port>` \
                 finds the missing pair, then pass the file with `--pairs-file <path>`"
            );
        }
    })
}

fn describe(ports: &PowerPorts) {
    println!("cutting {:?}", ports.primary());
    match ports.held() {
        [] => println!("   nothing held down (receptacle has no other half)"),
        held => println!(
            "   holding down {}",
            held.iter()
                .map(|p| format!("{p:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Find the other half of `hub`'s receptacles by watching the power LED of
/// whatever is plugged into its `port`; prints the line for the pairs file.
fn probe(hub: &str, port: u8) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let confirm = |question: &str| -> std::io::Result<bool> {
        print!("{question}");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        stdin.lock().read_line(&mut answer)?;
        Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
    };
    println!("probing {hub} port {port} - watch the power LED of the device on it");
    powercycling::probe(hub, port, OFF_TIME, &mut out, confirm)?;
    Ok(())
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
fn primary_only(device: &DeviceId, pairs: &HubPairs) -> powercycling::Result<()> {
    let ports = find(device, pairs)?;
    println!(
        "cutting ONLY {:?}, leaving {} port(s) powered",
        ports.primary(),
        ports.held().len()
    );
    println!("watch the board's power LED for {OFF_TIME:?}...");

    ports.primary().set_power(false)?;
    std::thread::sleep(OFF_TIME);
    ports.primary().set_power(true)?;

    powercycling::wait_for_device(device, Duration::from_secs(10))?;
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
fn verify(device: &DeviceId, pairs: &HubPairs) -> powercycling::Result<()> {
    let t = Instant::now();
    let ports = find(device, pairs)?;
    println!("found ports in {:?}", t.elapsed());
    describe(&ports);

    // Held-down ports are empty, unless the target is a hub: then one of them
    // feeds its other half, which drops along with it.
    let expected: Vec<String> = std::iter::once(ports.primary())
        .chain(ports.held())
        .map(HubPort::child_location)
        .filter(|loc| {
            std::path::Path::new("/sys/bus/usb/devices")
                .join(loc)
                .exists()
        })
        .collect();
    println!(
        "expecting only {} (and anything below) to drop\n",
        expected.join(", ")
    );

    println!("watch the board's power LED: dark => VBUS was cut, lit => only the link dropped\n");

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
    powercycling::wait_for_device(device, Duration::from_secs(10))?;
    std::thread::sleep(Duration::from_millis(750));
    let after = bus_snapshot();

    let intended = |d: &String| {
        expected
            .iter()
            .any(|e| d == e || d.starts_with(&format!("{e}.")))
    };

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
