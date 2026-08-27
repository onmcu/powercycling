# powercycling

Power-cycle a USB device identified by `vid:pid:serial`, by switching hub port
power (PPPS). Linux only.

Built for hardware-in-the-loop rigs, where a hanging MCU devboard needs its VBUS
cut rather than only the USB link reset. The crate cuts exactly the one port
that feeds your device, holds the other half of its receptacle down with it
(USB 3.x receptacles have two), and refuses — with an error that says what to
do — whenever it cannot be sure that only your device loses power.

## 1. Cycle a device

**Hardware:** the device hangs off a hub that does per-port power switching
(PPPS). Most bare-board hubs do; many consumer hubs claim it but do not act on
it — step 2 finds that out.

**Permissions:** switching a port needs write access to the port's sysfs
`disable` attribute (kernel 6.0+) or the hub's usbfs node. Install uhubctl's
[`52-usb.rules`](https://github.com/mvp/uhubctl/blob/master/udev/rules.d/52-usb.rules)
and re-plug the hub. A missing rule shows up as `Error::HubUnreadable`.

**Identity:** `lsusb` gives `vid:pid`; the serial is in
`/sys/bus/usb/devices/<location>/serial`. Without a serial, `vid:pid` must be
unique on the bus.

```rust,no_run
use powercycling::{DeviceId, HubPairs};
use std::time::Duration;

fn main() -> Result<(), powercycling::Error> {
    let device = DeviceId::new(0x0483, 0x374e, Some("0050003A3233511639363634"));
    // Off for 2 s, then up to 10 s for the device to re-enumerate. `HubPairs`
    // is what your machine needs declared about its hubs - usually nothing;
    // step 3 tells you if it does.
    powercycling::power_cycle(
        &device,
        &HubPairs::none(),
        Duration::from_secs(2),
        Duration::from_secs(10),
    )?;
    Ok(())
}
```

The same from the command line, with the bundled example:

```text
cargo run --example cycle -- 0483 374e 0050003A3233511639363634
```

## 2. Check it once per rig

`Ok` means the device left the bus and came back. It does **not** prove the
device lost power: a port that is switched off disables its link, so the device
vanishes whether VBUS dropped or not, and nothing over USB tells the two apart.
Check once, with your eyes on the board's power LED (or a meter):

```text
cargo run --example cycle -- <vid> <pid> <serial> --verify
```

`--verify` cuts the power, reports what left the bus — flagging anything that
was not your device as **COLLATERAL** — and confirms everything came back.
While it runs, watch the LED:

| LED during `--verify` | meaning | do |
|---|---|---|
| dark, and no collateral | power really was cut | done |
| dark, collateral listed | something else lost power too | step 3, `BehindHub` |
| stays lit | the port was disabled, VBUS was not | step 3, "LED stays lit" |

Once a rig passes this, it stays passed: the pairing is derived from the
topology, not from luck.

## 3. When it does not work

Every failure is an error that names the reason. This is the escalation
ladder, from "fix the call" to "fix the hardware".

**`NotFound` / `Ambiguous`** — nothing or several things match. Check `lsusb`;
add the serial.

**`HubUnreadable`** — the hub would not open. Permissions, see step 1.

**`NoSwitchableHub`** — the device is plugged straight into the machine. Root
hub ports are host controller ports, outside the specification's hub chapter,
and the crate does not switch them. Put a PPPS hub between the machine and the
device.

**`BehindHub`** — the device's own hub does not switch power per port (chained
hubs usually report *ganged*), and the port that does sits above that hub and
feeds it whole. The crate refuses to take every device on that hub down on
your behalf. The error names the hub and its `vid:pid[:serial]`. Two ways out:

- put the device on a PPPS hub instead, or
- if that hub *is* the unit you want cycled — a PCB with its own hub and
  several devices on it — target the hub explicitly:

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

A USB 3.x hub is two logical devices, one per half of its receptacle (see
[background](#how-it-works)), usually with different PIDs. `BehindHub` names the
half on the device's bus; either half works as the target.

**`HubUnpaired`** — the device's hub is one half of a USB 3.x hub, and the crate
could not tell which hub is the other half. Both halves keep the receptacle
powered, so cutting one alone would only disconnect the device; the crate cuts
nothing. This happens on boards that route the two halves through unrelated
hubs — on a Raspberry Pi CM5 IO board the USB 2.0 side goes through an on-board
hub while the `SuperSpeed` side goes straight to the controller. The fix is one
declaration, made once per machine:

1. See the pairing and why this hub is unpaired:
   ```text
   cargo run --example cycle -- --pairs
   ```
2. Plug something with a power LED into the unpaired hub and find its other
   half. The probe cuts the port alone, then together with each candidate,
   asking before every cut and restoring after, and prints the line to declare:
   ```text
   cargo run --example cycle -- --probe 2-1.2 4
   ```
3. Declare it. The crate reads no file on its own; keep the pairs wherever your
   configuration lives and pass them in:
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
   The example takes `--pairs-file cm5.pairs`. One line covers every hub
   chained below the pair, however many identical ones there are. A hub whose
   receptacles have no other half at all is declared as `<hub> none`.

**`PeerNotSwitchable`** — the other half was identified but switches power in
ganged mode, so it cannot be cut without taking its neighbours. This receptacle
cannot be power-cycled per port; use a different hub.

**`PeerNotFound`** — the other half's port holds a device of its own, which one
receptacle cannot. The pairing is wrong: check `--pairs`, and declare the right
one as above.

**`PowerOffIneffective`** — the port was switched off but the device is still
enumerated. The hub accepted `PORT_POWER` without acting on it. Use a hub that
does.

**LED stays lit, but `Ok`** — the port was disabled and VBUS was not cut. Either
the hub does not gate VBUS on that port (check with a meter), or — to make sure
the hold-down of the other half is doing its job — run

```text
cargo run --example cycle -- <vid> <pid> <serial> --primary-only
```

which deliberately cuts your device's port alone. LED dark ⇒ this hub gates
VBUS on one port and the other half never mattered. LED lit ⇒ the other half
matters, and the pairing (`--pairs`) is what to look at.

**Anything else** — `--debug` prints what every stage of the search sees.

## Finer control

`PowerPorts::find` returns the ports and `PowerPorts` switches them:

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

Find the ports *before* cutting: once VBUS drops, the device can no longer be
looked up. `PowerPorts::cycle` cuts, waits, checks the device is gone, and
restores — always, so a failure never leaves a port held down. When a
`SuperSpeed` port is involved the off period is stretched to at least 200 ms,
as such a port's power-off is not immediate; the minimum is absorbed into the
caller's off time, never added to it.

## Command-line reference

```text
cargo run --example cycle -- <vid> <pid> [serial]                 # cycle
cargo run --example cycle -- <vid> <pid> [serial] --verify        # step 2: check for collateral damage
cargo run --example cycle -- <vid> <pid> [serial] --primary-only  # cut one half only, watch the LED
cargo run --example cycle -- <vid> <pid> [serial] --debug         # what the search sees
cargo run --example cycle -- --pairs                              # every hub and its other half
cargo run --example cycle -- --probe <hub> <port>                 # find a missing pair by LED
```

Each takes `--pairs-file <path>` for the machine's declared pairs.

## How it works

### What gets switched

`PowerPorts::find` takes the port on the hub directly above the device and
requires that hub to do per-port power switching. A hub without switches
(*ganged*) still *accepts* `PORT_POWER` and answers it by disabling the port —
the device vanishes from the bus, VBUS untouched, the device powered all along.

Hubs chained behind a PPPS-capable hub usually report ganged switching. The
port that does cut VBUS then sits further up and feeds the chained hub whole,
so cutting it would take down every device on that hub. `find` refuses
(`BehindHub`) rather than doing that on your behalf; targeting the hub itself
does it deliberately.

### Why both halves of a USB 3.x receptacle matter

A USB 3.x receptacle is one physical socket carrying two USB links: a USB 2.0
hub and a `SuperSpeed` hub each own a port on it. A device uses only one of
them and leaves the other reading empty. (A USB 3.x *hub* occupies both — its
two halves are two devices.)

The socket has a single VBUS pin, and the two ports feed it like switches wired
in parallel:

> Typically, the only signal shared logic between them is to control VBUS. **If
> either the USB 2.0 hub or Enhanced SuperSpeed hub controllers requires a
> downstream port to be powered, power is turned on for the port.** (§10.1)

Table 10-2 makes it normative: for a hub connected upstream, VBUS reads "On"
wherever one half has `PORT_POWER = 1`, and may only be off when both halves sit
at 0. Cutting only the half your device sits on does **not** cut power; it only
drops the device off the bus: the debug session dies, the MCU keeps running.
**Both halves have to be down at once.**

### Which port is the other half

Port *numbers* are known: both halves of a hub number their ports alike.

> The port numbers assigned to a specific port by the hub shall be consistent
> between the USB 2.0 hub and Enhanced SuperSpeed hub. (§10.3.3)

So the question is never "which port" but "which *hub* is the other half of
this one". Hubs are paired by, in order of trust:

1. **What the caller declares** in a `HubPairs`.
2. **Same host controller** — the two root hubs of one xHCI controller.
3. **Descent** — a hub on port N of a paired hub is paired with the hub on port
   N of its partner, provided the two are of opposite speeds, the same vendor
   and the same size.

Once a hub is paired, so is everything chained below it, and the held-down port
is simply port N of the partner. `--pairs` prints the table:

```text
-- hubs and their other halves
   usb2       USB 2.00  1d6b:0002   2 ports  ppps    <-> usb3       same host controller
   2-1        USB 2.00  2109:2817   4 ports  ppps    no other half (USB 2.0 hub)
   2-1.2      USB 2.10  0bda:5411   4 ports  ppps    <-> 3-2        declared
   2-1.2.3    USB 2.10  0bda:5411   4 ports  ppps    <-> 3-2.3      port 3 of paired hubs
   usb3       USB 3.00  1d6b:0003   2 ports  ppps    <-> usb2       same host controller
   3-2        USB 3.20  0bda:0411   4 ports  ppps    <-> 2-1.2      declared
   3-2.3      USB 3.20  0bda:0411   4 ports  ppps    <-> 2-1.2.3    port 3 of paired hubs
```

Rules 2 and 3 cover a hub plugged into a receptacle whose two halves share
ancestry — every PC, and every hub chained below a paired one. They cannot cover
a board that routes the two halves through unrelated hubs, which is the CM5
case from step 3:

```text
USB 2.0:    usb2 port 1 → 2-1 (USB 2.0 only) → 2-1.2 → 2-1.2.3 → device on port 4
SuperSpeed:                        usb3 port 2 → 3-2 → 3-2.3 → port 4 empty   ← the peer
```

Nothing on the bus says that port X of `2-1` and port X of `usb3` are one
receptacle: `2-1` has no `SuperSpeed` half, and its `SuperSpeed` counterpart is a
root hub one level up. Hence the declaration `2-1 usb3`, and rule 3 does the
rest.

The kernel's own `peer` links are *not* used. They are built by rule 3 without
the sanity checks, and on a board like this they pair the wrong hubs — and
every hub underneath inherits the mistake.

### When the device is a hub

A USB 3.x hub plugged into a USB 3.x receptacle occupies *both* halves: its
USB 2.0 hub on one port, its `SuperSpeed` hub on the other (§10.1). So when the
target is a hub, the held-down port is expected to hold a hub, and it is cut
along with the target's own — that is how a hub and everything on it gets
cycled. For any other device the other half must read empty; if it does not,
the pairing is wrong and `find` fails with `PeerNotFound` rather than cutting a
port that feeds something else.

### Hubs only, not host controller ports

Root hub ports are host controller ports, which the specification's hub chapter
excludes:

> All exposed downstream ports on a USB hub shall support both Enhanced
> SuperSpeed and USB 2.0 connections. **Host controller ports may have different
> requirements.** (§10.1)

So a device plugged straight into the machine yields `NoSwitchableHub`. (On the
Linux machines tried so far, root hub ports do carry a kernel `peer` link, so
root receptacles could in principle be handled; the crate does not, by choice.)

### What cannot be checked

`PowerPorts::cycle` checks that the device left the bus before restoring power
and returns `PowerOffIneffective` if it did not: a powered-off port holds its
link in `eSS.Disabled` (§10.3.1.1), so a device still enumerated means the hub
accepted `PORT_POWER` without acting on it.

The converse cannot be checked over USB. Table 10-2 says VBUS "May be off" when
both halves are down — not that it shall be — and a hub that keeps it on to
support power applications from the port conforms. That is what step 2 is for.

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
