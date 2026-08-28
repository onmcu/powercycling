//! Power-cycle one device from the command line.
//!
//!     cargo run -- 0483 374e 0050003A3233511639363634
//!     cargo run -- 0483 374e 0050003A3233511639363634 --debug
//!     cargo run -- 0483 374e 0050003A3233511639363634 --verify
//!     cargo run -- 0483 374e 0050003A3233511639363634 --primary-only
//!     cargo run -- --tree
//!     cargo run -- --probe 2-1.2 4
//!
//! Every form takes `--pairs-file <path>` for the hub pairs the machine needs
//! declared (see `HubPairs`); without it, none are. `--above N` targets the
//! hub N levels above the device instead - `--above 1` cycles the carrier
//! board the device sits on, and everything else on it.

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

    // `--pairs-file <path>` and `--above <n>` anywhere in the arguments.
    let pairs = take_option(&mut args, "--pairs-file")?
        .map_or_else(|| Ok(HubPairs::none()), HubPairs::load)?;
    let above: u8 = take_option(&mut args, "--above")?.map_or(Ok(0), |n| n.parse())?;

    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["--tree"] => return Ok(powercycling::tree(&pairs, &mut std::io::stdout())?),
        ["--probe", hub, port] => return probe(hub, port.parse()?, &pairs),
        _ => {}
    }
    let [vid, pid, rest @ ..] = args.as_slice() else {
        eprintln!(
            "usage: <vid-hex> <pid-hex> [serial] [--debug|--verify|--primary-only]\n       \
             --tree\n       \
             --probe <hub> <port>\n       \
             each with an optional --pairs-file <path>, the first also --above <n>"
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
        return Ok(verify(&device, above, &pairs)?);
    }
    if has("--primary-only") {
        return Ok(primary_only(&device, above, &pairs)?);
    }

    let t0 = Instant::now();
    let ports = find(&device, above, &pairs)?;
    let found = t0.elapsed();
    println!("{ports}");

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

/// Remove `--flag <value>` from `args` and return the value, if present.
fn take_option(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
    let Some(i) = args.iter().position(|a| a == flag) else {
        return Ok(None);
    };
    if i + 1 >= args.len() {
        return Err(format!("{flag} needs a value"));
    }
    let value = args.remove(i + 1);
    args.remove(i);
    Ok(Some(value))
}

/// `PowerPorts::find_above`, with the command-line way out spelled out for
/// the errors whose fix is a flag rather than a change of hardware.
fn find(device: &DeviceId, above: u8, pairs: &HubPairs) -> powercycling::Result<PowerPorts> {
    PowerPorts::find_above(device, above, pairs).inspect_err(|e| match e {
        Error::BehindHub { hub, levels, .. } => eprintln!(
            "hint: add `--above {}` to cycle {hub} and everything on it",
            above + levels
        ),
        Error::HubUnpaired { hub, .. } => eprintln!(
            "hint: `--tree` shows the hubs and their pairing, `--probe {hub} \
             <port>` finds the missing pair, then pass the file with `--pairs-file <path>`"
        ),
        _ => {}
    })
}

/// Find the other half of `hub`'s receptacles by watching the power LED of
/// whatever is plugged into its `port`; prints the line for the pairs file.
fn probe(hub: &str, port: u8, pairs: &HubPairs) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let ask = |prompt: &str| -> std::io::Result<String> {
        print!("{prompt}");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        stdin.lock().read_line(&mut answer)?;
        Ok(answer)
    };
    powercycling::probe(hub, port, OFF_TIME, pairs, &mut out, ask)?;
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
fn primary_only(device: &DeviceId, above: u8, pairs: &HubPairs) -> powercycling::Result<()> {
    let ports = find(device, above, pairs)?;
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
fn verify(device: &DeviceId, above: u8, pairs: &HubPairs) -> powercycling::Result<()> {
    let t = Instant::now();
    let ports = find(device, above, pairs)?;
    println!("found ports in {:?}", t.elapsed());
    println!("{ports}");

    let before = bus_snapshot();
    // Held-down ports are empty, unless the target is a hub: then one of them
    // feeds its other half, which drops along with it.
    let expected: Vec<String> = std::iter::once(ports.primary())
        .chain(ports.held())
        .map(HubPort::child_location)
        .filter(|loc| before.contains(loc))
        .collect();
    println!(
        "expecting only {} (and anything below) to drop\n",
        expected.join(", ")
    );

    println!("watch the board's power LED: dark => VBUS was cut, lit => only the link dropped\n");

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
