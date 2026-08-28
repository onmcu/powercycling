# powercycling

Power-cycle a USB device identified by VID (Vendor ID), PID (Product ID),
and serial number, by switching hub port power (PPPS). Linux only.

Built for hardware-in-the-loop rigs, where a hanging MCU devboard needs its VBUS
cut rather than only the USB link reset. The crate cuts exactly the one port
that feeds the device. For USB 3.x hubs, it also holds the other half of the
receptacle down with it.

When it cannot be ensured that only your device loses power, an error with
precise instructions is returned.

## 1. Cycle a device

**Hardware:** the device hangs off a hub that does per-port power switching
(PPPS). Most bare-board hubs do; many consumer hubs _claim_ it but do not
have the hardware (MOSFETs) to actually cut the power (step 2 finds that out).

**Permissions:** switching a port needs write access to the port's sysfs
`disable` attribute (kernel 6.0+) or the hub's usbfs node.
Install uhubctl's [`52-usb.rules`](https://github.com/mvp/uhubctl/blob/master/udev/rules.d/52-usb.rules)
and re-plug the hub. A missing rule shows up as `Error::HubUnreadable`.

**Identity:** `lsusb` gives VID and PID; the serial is in
`/sys/bus/usb/devices/<location>/serial`.
The crate can work without a serial if the VID-PID pair is unique on the bus.

```rust,no_run
use powercycling::{DeviceId, HubPairs};
use std::time::Duration;

fn main() -> Result<(), powercycling::Error> {
    let device = DeviceId::new(0x0483, 0x374e, Some("0050003A3233511639363634"));
    // Off for 2 s, then up to 10 s for the device to re-enumerate. `HubPairs`
    // is what your machine needs declared about its hubs. Usually nothing, but
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

The same from the command line, using the program in `main.rs`:

```text
cargo run -- 0483 374e 0050003A3233511639363634
```

## 2. Check it once per setup

`Ok` means the device left the bus and came back. It does **not** (and cannot!)
prove the device lost power: a port that is switched off disables its link,
so the device disappears – no matter if VBUS really dropped or not.
Nothing over USB tells the two apart.

So do this check once with a device that has an LED lighting up when the USB
port is powered and confirm visually that power is actually being cut:

```text
cargo run -- <vid> <pid> <serial> --verify
```

`--verify` cuts the power, reports what left the bus (flagging anything that
was not your device as **COLLATERAL**) and confirms everything came back.

While it runs, watch the LED:

| LED during `--verify` | meaning | do |
|---|---|---|
| dark, and no collateral | power really was cut | done |
| dark, collateral listed | something else lost power too | step 3, `BehindHub` |
| stays lit | the port was disabled, VBUS was not | step 3, "LED stays lit" |

Once a setup passes this, it stays passed: the pairing is derived from the
topology, not from guesswork.

## 3. When it does not work

Every failure is an error that names the reason.

The following is the escalation ladder, from "fix the call" to "fix the hardware".

- **`NotFound` / `Ambiguous`:** nothing or several devices match. Check `lsusb` or
  run `main.rs` with the `--tree` flag. If necessary, add the serial.

- **`HubUnreadable`:** the hub does not open. Likely permissions, see step 1.

- **`NoSwitchableHub`:** the device is plugged directly into the machine. Root
  hub ports are host controller ports and therefore outside the specification's hub
  chapter, so the crate does not switch them (yet).
  _Fix:_ Put a PPPS hub between the machine and the device.

- **`BehindHub`:** the device's direct "ancestor hub" does not switch power per port
  (chained hubs usually report _ganged_). Above that hub sits a port that _does_ switch,
  but it may feed other devices. The crate refuses to take every device connected to that
  hub down on your behalf. The error names the hub and how many levels up it is; `main.rs`
  prints the exact `--above N` to add.

  _Fix:_

  - put the device on a PPPS hub directly, or
  - if that hub _is_ the unit you want cycled, e.g., a carrier board with its own
    hub, a devboard and measurement hardware on it, explicitly tell the crate how many hubs
    to go up:

    ```rust,no_run
    use powercycling::{DeviceId, HubPairs, PowerPorts};
    use std::time::Duration;
    
    fn main() -> Result<(), powercycling::Error> {
        let mcu = DeviceId::new(0x0483, 0x374e, Some("0050003A3233511639363634"));
        let pairs = HubPairs::none();
        // The MCU alone (if its hub does PPPS) ...
        PowerPorts::find(&mcu, &pairs)?.cycle(Duration::from_secs(2))?;
        // ... or the whole carrier: the hub one level up, with everything on it.
        PowerPorts::find_above(&mcu, 1, &pairs)?.cycle(Duration::from_secs(2))?;
        powercycling::wait_for_device(&mcu, Duration::from_secs(10))?;
        Ok(())
    }
    ```

    For the same behavior on the command line, add `--above 1`.
    Naming the carrier through the MCU's serial is deliberate: identical carriers may have
    identical hubs with no serials of their own, so the hub's VID/PID alone would be `Ambiguous`.

- **`HubUnpaired`:** the device's hub is one half of a USB 3.x hub, and the crate
  cannot tell which hub is the other half. Both halves keep the receptacle powered,
  so cutting one alone would only disconnect the device. The crate cuts nothing.
  This happens on boards that route the two halves in a way the crate's rules
  (see [How it works](#how-it-works)) do not recognize.

  The fix is one declaration, usually only necessary once per machine:

  1. Check the tree: which hub is paired with which – and why this one is not:
    ```text
    cargo run -- --tree
    ```
  2. Plug something with a power LED into the unpaired hub and find its other
    half:
    ```text
    cargo run -- --probe 2-1.2 4
    ```
    The probe prints its plan: first, cycle the port alone, then together with each
    candidate, most likely pair first.
    Then it runs the plan one step per `Enter`-press: cut, wait, restore, then ask
    "LED went dark?". Press `y` when it did; the probe prints the line to declare.
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
   `main.rs` takes `--pairs-file cm5.pairs`.
   One line covers every hub chained below the pair, no matter how many identical
   ones there are. If a hub's receptacles have _no other half_ at all, declare it
   as `<hub> none`.

- **`PeerNotSwitchable`:** the other half was identified, but switches power in
  ganged mode, so it cannot be cut without taking its neighbours. This receptacle
  cannot be power-cycled per port.
  _Fix:_ use a different hub.

- **`PeerNotFound`:** the peer's port holds a device of its own, which one
  receptacle cannot (unless it is another hub). The pairing is probably wrong:
  check `--tree`, and declare the right pairing as described above.

- **`PowerOffIneffective`:** the port was switched off, but the device is still
  enumerated. The hub accepted `PORT_POWER`, but did not act on it.
  _Fix:_ use a hub that does.

- **`LED stays lit, but Ok`:** the port was disabled and VBUS was not cut. Either
  the hub does not gate VBUS on that port (measure), or the peer's port is not
  held down properly. To tell them apart:

  ```text
  cargo run -- <vid> <pid> <serial> --primary-only
  ```

  which deliberately cuts the device's port alone. LED dark ⇒ this hub gates
  VBUS on one port and the other half never mattered. LED lit ⇒ the other half
  matters, and the pairing (`--tree`) is what to look at.

- **Anything else:** `--debug` prints what every stage of the search sees.

## Finer control

`PowerPorts::find` returns the ports and `PowerPorts` switches them;
`PowerPorts::find_above` does the same for the hub N levels above the device:

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

Find the ports _before_ cutting: once VBUS drops, the device can no longer be
looked up. `PowerPorts::cycle` cuts, waits, checks that the device is gone, and
restores. It restores on failure, too, so no port is left held down. If a
`SuperSpeed` port is involved, the off time is at least 200 ms, since such a
port's power-off is not immediate.

## Command-line reference

```text
cargo run -- <vid> <pid> [serial]                 # cycle
cargo run -- <vid> <pid> [serial] --verify        # step 2: check for collateral damage
cargo run -- <vid> <pid> [serial] --primary-only  # cut one half only, watch the LED
cargo run -- <vid> <pid> [serial] --debug         # what the search sees
cargo run -- --tree                               # the USB tree, hubs with their other half
cargo run -- --probe <hub> <port>                 # find a missing pair by LED
```

Each takes `--pairs-file <path>` for the machine's declared pairs; the first
four also `--above <n>` to target the hub `n` levels above the device.

## How it works

### What gets switched

`PowerPorts::find` takes the port on the hub directly above the device and
requires that hub to do per-port power switching. A hub without switches
(_ganged_) still _accepts_ `PORT_POWER` and answers it by disabling the port.
The device disappears from the bus, but VBUS stays on: no power cycle.

Hubs chained behind a PPPS-capable hub usually report _ganged_ switching.
The port that does cut VBUS then sits further up and feeds the entire chain of hubs,
so cutting it would take down _every device on that hub_.
`find` refuses (`Error::BehindHub`) rather than doing that on your behalf. You can still
target the hub itself to take it down deliberately.

### Why both halves of a USB 3.x receptacle matter

A USB 3.x receptacle is one physical socket carrying two USB links: a USB 2.0
hub and a `SuperSpeed` hub each own a port on it. A device uses only one of
them and leaves the other reading empty. (A USB 3.x _hub_ occupies both; its
two halves are two devices.)

The socket has a single VBUS pin, and the two ports feed it like an OR gate:

> Typically, the only signal shared logic between them is to control VBUS. **If
> either the USB 2.0 hub or Enhanced SuperSpeed hub controllers requires a
> downstream port to be powered, power is turned on for the port.** (§10.1)

Table 10-2 makes it normative: for a hub connected upstream, VBUS reads "On"
wherever one half has `PORT_POWER = 1`, and may only be off when both halves sit
at 0. Cutting only the half your device sits on does **not** cut power; it only
drops the device off the bus: the debug session dies, the MCU keeps running.
**Both halves have to be down at once.**

### Which port is the other half

_Note: This is where it becomes really tricky and where things can go terribly wrong._

Port *numbers* are known: both halves of a hub number their ports alike.

> The port numbers assigned to a specific port by the hub shall be consistent
> between the USB 2.0 hub and Enhanced SuperSpeed hub. (§10.3.3)

So the question is never "which port" but "which *hub*" is the other half of
this one.

Since the port numbers' pairing is trivial, we need to _pair the hubs._
The crate does this in order of confidence:

1. **Declared:** what the caller says in a `HubPairs`.
2. **Same host controller:** the two root hubs of one xHCI controller are
   the two halves of its receptacles. True on every machine seen so far; the
   specification does not guarantee it.
3. **Expansion:** some boards route one side of their receptacles through a
   hub. The tell: one root of the controller has a *single* port, and on it
   hangs a hub with exactly as many ports as the other root (and no twin of
   its own on the bus):

   ```text
   usb2 (1 port) ── 2-1 (4 ports) ─ port 1 ─ port 2 ─ port 3 ─ port 4
                                       │        │        │        │     the same four receptacles
   usb3 (4 ports) ───────────────── port 1 ─ port 2 ─ port 3 ─ port 4
   ```

   Every receptacle needs a port on each side, and the only USB 2.0 ports
   there are belong to that hub. So it stands in for the small root:
   `2-1 <-> usb3`, port N to port N. (The same with the sides swapped.)
4. **Descent:** paired hubs have paired ports (§10.3.3), so the hub on port N
   of one is the other half of the hub on port N of its peer. Walking down from
   a paired hub-pair, this finds every hub below it, three identical ones or not.
   Before pairing, the two are checked for what the halves of one chip must
   share: opposite speeds, the same vendor, the same number of ports. The check
   finds nothing on its own; it only rejects. A hub that fails it stays unpaired
   (`HubUnpaired`) rather than being paired with a wrong hub.

Once a hub is paired, so is everything chained below it, and the held-down port
is simply port N of the peer.
The `--tree` command, similar to a `lsusb -t` with the things that matter here, shows it:
switching mode, other half and how it was found, and what sits on every port.

A Raspberry Pi CM5 IO board with one Realtek hub in receptacle 4 and two more chained below it:

```text
usb2         1d6b:0002  "xHCI Host Controller"  USB 2.00  1 ports  ppps    same host controller as usb3; 2-1 stands in for it
└─ port 1: 2-1          2109:3431  "USB2.0 Hub"  USB 2.10  4 ports  ganged  <-> usb3 (expands usb2's single port to 4)
   ├─ port 1: -
   ├─ port 2: -
   ├─ port 3: -
   └─ port 4: 2-1.4        0bda:5411  "USB2.1 Hub"  USB 2.10  4 ports  ppps    <-> 3-4 (port 4 of paired parents)
      ├─ port 1: 2-1.4.1      1a40:0101  "USB 2.0 Hub"  USB 2.00  4 ports  ganged  no other half (USB 2.0 hub)
      │  ├─ port 1: 2-1.4.1.1    0483:374e  "STLINK-V3"  serial 0050003A3233511639363634
      │  ├─ port 2: -
      │  ├─ port 3: -
      │  └─ port 4: -
      ├─ port 2: -
      ├─ port 3: 2-1.4.3      0bda:5411  "USB2.1 Hub"  USB 2.10  4 ports  ppps    <-> 3-4.3 (port 3 of paired parents)
      │  └─ …
      └─ port 4: 2-1.4.4      0bda:5411  "USB2.1 Hub"  USB 2.10  4 ports  ppps    <-> 3-4.4 (port 4 of paired parents)
         └─ …
usb3         1d6b:0003  "xHCI Host Controller"  USB 3.00  4 ports  ppps    <-> 2-1 (expands usb2's single port to 4)
├─ port 1: -
├─ port 2: -
├─ port 3: -
└─ port 4: 3-4          0bda:0411  "USB3.2 Hub"  USB 3.20  4 ports  ppps    <-> 2-1.4 (port 4 of paired parents)
   ├─ port 1: -
   ├─ port 2: -
   ├─ port 3: 3-4.3        0bda:0411  "USB3.2 Hub"  USB 3.20  4 ports  ppps    <-> 2-1.4.3 (port 3 of paired parents)
   │  └─ …
   └─ port 4: 3-4.4        0bda:0411  "USB3.2 Hub"  USB 3.20  4 ports  ppps    <-> 2-1.4.4 (port 4 of paired parents)
      └─ …
```

**How to read it:** the USB 2.0 side of the controller (`usb2`) has a single port.
This port feeds a 4-port hub whose own `SuperSpeed` half is nowhere seen on the bus.
So the CM5 IO board runs the USB 2.0 lines of its four receptacles through that hub,
but `SuperSpeed` lines go straight to the host controller's (`usb3`) four ports (rule 3).

Below that, every hub pairs with its twin by port number (rule 4), three identical
hubs or not.

The STLINK on `2-1.4.1.1` sits on a plain USB 2.0 hub that is ganged, so cycling it
alone is refused (`BehindHub`); `--above 1` cycles that hub (the carrier) through
`2-1.4 port 1` and `3-4 port 1` together.

What rule 4's check guards against is a hub spliced into one side only,
without rule 3's single-port tell:

```text
usb2 (4 ports) ─ port 2 ─ 2-2 (4 ports) ─ port 1 ─ port 2 ─ port 3 ─ port 4
                                              │        │        │        │    the same four receptacles
usb3 (4 ports) ───────────────────────── port 1 ─ port 2 ─ port 3 ─ port 4
```

On the bus, `2-2` looks like an ordinary hub in receptacle 2, and rule 4 would
pair it with whatever hub sits on `usb3` port 2. The check rejects that unless
it is a look-alike hub. This board needs a declaration: `2-2 usb3`.

The kernel's own `peer` links are _not_ used. They are rule 4 without the
check, and on the board of rule 3 they pair the wrong hubs:

```text
$ readlink usb3/3-0:1.0/usb3-port1/peer
../../../usb2/2-0:1.0/usb2-port1
```

This says `usb3 <-> usb2`; in practice it is `usb3 <-> 2-1`. Every hub below
would inherit the mistake.

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

So a device plugged straight into the machine yields `NoSwitchableHub`.

### What cannot be checked

`PowerPorts::cycle` checks that the device left the bus before restoring power
and returns `PowerOffIneffective` if it did not: a powered-off port holds its
link in `eSS.Disabled` (§10.3.1.1), so a device still enumerated means the hub
accepted `PORT_POWER` without acting on it.

The converse cannot be checked over USB. Table 10-2 says VBUS "May be off" when
both halves are down, not that it shall be. A hub that keeps it on conforms.
That is what step 2 is for.

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
