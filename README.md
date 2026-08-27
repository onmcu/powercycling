# powercycling

Power-cycle a USB device identified by `vid:pid:serial`, by switching hub port
power (PPPS). Linux only.

Built for hardware-in-the-loop rigs, where a hanging MCU devboard needs its VBUS
cut rather than only the USB link reset.

```rust,no_run
use powercycling::{DeviceId, HubPairs};
use std::time::Duration;

fn main() -> Result<(), powercycling::Error> {
    let device = DeviceId::new(0x0483, 0x374e, Some("0050003A3233511639363634"));
    // Off for 2 s, then up to 10 s for the device to re-enumerate. `HubPairs`
    // is what the machine needs declared about its hubs - usually nothing.
    powercycling::power_cycle(
        &device,
        &HubPairs::none(),
        Duration::from_secs(2),
        Duration::from_secs(10),
    )?;
    Ok(())
}
```

For finer control, use `PowerPorts::find` to obtain the `PowerPorts` that need
to be switched in order to cut power to the device:

```rust,no_run
use powercycling::{DeviceId, HubPairs, PowerPorts};
use std::{thread::sleep, time::Duration};

fn main() -> Result<(), powercycling::Error> {
    let ports = PowerPorts::find(&DeviceId::new(0x0483, 0x374e, None), &HubPairs::none())?;
    ports.set_power(false)?; // hold the device off
    sleep(Duration::from_secs(2));
    ports.set_power(true)?;
    Ok(())
}
```

## What gets switched

`PowerPorts::find` finds the port on the hub directly above the device and 
ensures that hub does per-port power switching (PPPS).
A hub without switches (*ganged*) still *accepts* `PORT_POWER` and answers it by
disabling the port.
But this only causes the device to vanish from the bus, leaving VBUS untouched and
the device powered on all the time.

Hubs chained behind a PPPS-capable hub usually report *ganged* switching.
The port that does cut VBUS sits further up and feeds *all* the chained hubs, so
cutting power would take down _every device on that hub_.

In this case, `find` refuses with `Error::BehindHub` – rather than taking down unrelated
devices on your behalf.
The error names the hub and its `vid:pid[:serial]`.

If you really want to cycle the hub and everything on it, you must explicitly target the hub:

```rust,no_run
use powercycling::{DeviceId, Error, HubPairs, PowerPorts};

fn main() -> Result<(), Error> {
    let mcu = DeviceId::new(0x0483, 0x374e, None);
    let pairs = HubPairs::none();
    let ports = match PowerPorts::find(&mcu, &pairs) {
        // The MCU shares a ganged hub with other boards: cycle the hub instead.
        Err(Error::BehindHub { hub_id: Some(hub), .. }) => PowerPorts::find(&hub, &pairs)?,
        found => found?,
    };
    ports.cycle(std::time::Duration::from_secs(2))
}
```

A USB 3.x hub is **two** logical devices, one per half of its receptacle (see
below), and the two usually carry different PIDs. `BehindHub` names the half on
the device's bus; either half works as the target.

## Hubs only, not host controller ports

The walk stops below the root hub, so a device plugged straight into the machine
might yield `Error::NoSwitchableHub`.
Root hub ports are host controller ports, which the specification's hub chapter
excludes:

> All exposed downstream ports on a USB hub shall support both Enhanced
> SuperSpeed and USB 2.0 connections. **Host controller ports may have different
> requirements.** (§10.1)

Nothing there relates the port numbers of a machine's two root hubs, so the
other half of a root receptacle cannot be identified by the rule below. Put the
device behind a hub that does PPPS if you want to be sure.

**Note:** Some preliminary tests on various Linux computers suggest that for
root hub ports, the `peer` is always set in sysfs. That would make it possible
to identify the other half of a root receptacle by looking at the `peer` of the
root hub port.

## Why both halves of a USB 3.x port matter

A USB 3.x receptacle is one physical socket carrying two USB links: a USB 2.0
hub and a `SuperSpeed` hub each own a port on it. A device uses only one of
them and leaves the other reading empty.
(A USB 3 / `SuperSpeed` hub, however, would occupy both, making this a little
harder to generalize.)

The socket has a single VBUS pin, and the two ports feed it like switches wired
in parallel:

> Typically, the only signal shared logic between them is to control VBUS. **If
> either the USB 2.0 hub or Enhanced SuperSpeed hub controllers requires a
> downstream port to be powered, power is turned on for the port.** (§10.1)

Table 10-2 makes it normative: for a hub connected upstream, VBUS reads "On"
wherever one half has `PORT_POWER = 1`, and may only be off when both halves sit
at 0.
Cutting only the half your device sits on does **not** cut power, but only drops
it off the bus: the debug session dies, the MCU keeps running.
**Both halves have to be down at once.**

### Which port is the other half

Port *numbers* are known: both halves of a hub number their ports alike.

> The port numbers assigned to a specific port by the hub shall be consistent
> between the USB 2.0 hub and Enhanced SuperSpeed hub. (§10.3.3)

So the question is never "which port" but "which *hub* is the other half of
this one". Hubs are paired by, in order of trust:

1. **What the caller declares** in a `HubPairs` (below).
2. **Same host controller** — the two root hubs of one xHCI controller.
3. **Descent** — a hub on port N of a paired hub is paired with the hub on port
   N of its partner, provided the two are of opposite speeds, the same vendor
   and the same size.

Once a hub is paired, so is everything chained below it, and the held-down port
is simply port N of the partner. `--pairs` prints the table:

```text
-- hubs and their other halves
   usb2       USB 2.00  1d6b:0002   1 ports  ppps    <-> usb3       same host controller
   2-1        USB 2.00  2109:2817   4 ports  ppps    no other half (USB 2.0 hub)
   2-1.2      USB 2.10  0bda:5411   4 ports  ppps    <-> 3-2        declared
   2-1.2.3    USB 2.10  0bda:5411   4 ports  ppps    <-> 3-2.3      port 3 of paired hubs
   usb3       USB 3.00  1d6b:0003   2 ports  ppps    <-> usb2       same host controller
   3-2        USB 3.20  0bda:0411   4 ports  ppps    <-> 2-1.2      declared
   3-2.3      USB 3.20  0bda:0411   4 ports  ppps    <-> 2-1.2.3    port 3 of paired hubs
```

The kernel's own `peer` links are *not* used. They are built by rule 3 without
the sanity checks, and on a board like the one below they pair the wrong hubs —
and every hub underneath inherits the mistake.

### When a pair has to be declared

Rules 2 and 3 cover a hub plugged into a receptacle whose two halves share
ancestry — every PC, and every hub chained below a paired one. They cannot cover
a board that routes the two halves through unrelated hubs. On a Raspberry Pi
CM5 IO board the USB 2.0 side of each receptacle goes through an on-board USB
2.0-only hub while the `SuperSpeed` side goes straight to the controller:

```text
USB 2.0:    usb2 port 1 → 2-1 (USB 2.0 only) → 2-1.2 → 2-1.2.3 → device on port 4
SuperSpeed:                        usb3 port 2 → 3-2 → 3-2.3 → port 4 empty   ← the peer
```

Nothing on the bus says that port X of `2-1` and port X of `usb3` are one
receptacle: `2-1` has no `SuperSpeed` half, and its `SuperSpeed` counterpart is a
root hub one level up. So a hub plugged into such a receptacle (`2-1.2` here)
cannot be paired, and `find` fails with `Error::HubUnpaired` rather than cutting
one half and leaving the device powered. The error says what to do; the whole
fix is one declaration, made once per machine. It is the caller's to keep —
the crate reads no file of its own — as a file, text, or built in code:

```text
# cm5.pairs - one line per pair of hubs that share receptacles
2-1 usb3
```

```rust,no_run
use powercycling::HubPairs;

fn main() -> Result<(), powercycling::Error> {
    let from_file = HubPairs::load("cm5.pairs")?;
    let from_text: HubPairs = "2-1 usb3".parse()?;
    let built = HubPairs::none().pair("2-1", "usb3");
    assert_eq!(from_file, from_text);
    assert_eq!(from_text, built);
    Ok(())
}
```

Everything below `2-1.2` is then derived by rule 3, however many identical hubs
hang there. Nothing about a VBUS drop is visible over USB, so the pair is found
by watching a power LED: plug something with one into the unpaired hub and run

```text
cargo run --example cycle -- --probe 2-1.2 4
```

It cuts the port alone, then together with each candidate in turn — asking
before every cut and restoring after — and prints the line to declare. A hub
whose receptacles have no other half at all is declared as `2-1.2 none`.

### When the device is a hub

A USB 3.x hub plugged into a USB 3.x receptacle occupies *both* halves: its
USB 2.0 hub on one port, its `SuperSpeed` hub on the other (§10.1). So when the
target is a hub, the held-down port is expected to hold a hub, and it is cut
along with the target's own — that is how a hub and everything on it gets
cycled. Either half's `vid:pid` names the hub. For any other device the other
half must read empty; if it does not, the pairing is wrong and `find` fails with
`Error::PeerNotFound` rather than cutting a port that feeds something else.

## Errors

Nothing succeeds silently. `Error` distinguishes a missing device, an ambiguous
identity (several devices match, and `vid:pid` without a serial picks none of
them), a chain with no switchable hub, a device behind a hub that would be cut
whole, a hub that could not be opened, a hub whose other half is unknown, a
ganged other half, a wrongly paired one, a malformed pairs file, and a device
still enumerated after power-off. Every message says what to do next.

When a `SuperSpeed` port is involved, `PowerPorts::cycle` stretches the off
period to at least 200 ms — such a port's power-off is not immediate. The
minimum is absorbed into the caller's off time, never added to it.

`PowerPorts::cycle` checks that the device left the bus before restoring power
and returns `Error::PowerOffIneffective` if it did not: a powered-off port holds
its link in `eSS.Disabled` (§10.3.1.1), so a device still enumerated means the
hub accepted `PORT_POWER` without acting on it.

The converse cannot be checked over USB alone. Table 10-2 says VBUS "May be off"
when both halves are down — not that it shall be — and a hub that keeps it on to
support power applications from the port conforms. Confirm with an LED or a
meter once per hardware setup — see `--primary-only` below.

## Permissions

Switching a port needs write access to either the port's sysfs `disable`
attribute (kernel 6.0+) or the hub's usbfs node. Use uhubctl's
[`52-usb.rules`](https://github.com/mvp/uhubctl/blob/master/udev/rules.d/52-usb.rules).
A missing rule surfaces as `Error::HubUnreadable`.

## Troubleshooting

```text
cargo run --example cycle -- <vid> <pid> <serial>                 # cycle
cargo run --example cycle -- <vid> <pid> <serial> --debug         # what the search sees
cargo run --example cycle -- <vid> <pid> <serial> --verify        # check for collateral damage
cargo run --example cycle -- <vid> <pid> <serial> --primary-only  # cut one half, watch the LED
cargo run --example cycle -- --pairs                              # every hub and its other half
cargo run --example cycle -- --probe <hub> <port>                 # find a missing pair by LED
```

Each takes `--pairs-file <path>` for the machine's declared pairs.

`--verify` samples sysfs while power is off and reports anything that dropped
which should not have. `--primary-only` deliberately skips the other half: if
the LED stays lit, the hold-down is doing real work on your hardware. `--pairs`
and `--probe` are for `Error::HubUnpaired`, see above.

## Specification references

Sections are USB 3.2 Revision 1.1 (June 2022) unless noted:

- **§10.1** — hub architecture: two logical hubs, VBUS as their only shared
  control; host controller ports carved out
- **§10.3.1.1, Table 10-2** — `DSPORT.Powered-off`, its disabled link, and when
  VBUS may be removed
- **§10.3.3** — downstream port numbering, consistent across both halves
- **USB 2.0 §11.23.2.1** — hub descriptor: `bNbrPorts` and `wHubCharacteristics`
  (logical power switching mode)
- **USB 2.0 §11.24.2, Table 11-17** — the `PORT_POWER` feature selector

## Credit

The approach — walking to a PPPS-capable hub, and the need to switch both halves
of a USB 3.x receptacle — comes from [uhubctl](https://github.com/mvp/uhubctl)
by Vadim Mikhailov, which is where this problem was solved first.

## License

MIT or Apache-2.0, at your option.
