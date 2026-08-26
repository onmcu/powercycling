# powercycling

Power-cycle a USB device identified by `vid:pid:serial`, by switching hub port
power (PPPS). Linux only.

Built for hardware-in-the-loop rigs, where a wedged MCU devboard needs its VBUS
cut rather than its USB link reset.

```rust,no_run
use powercycling::DeviceId;
use std::time::Duration;

fn main() -> Result<(), powercycling::Error> {
    let device = DeviceId::new(0x0483, 0x374e, Some("0050003A3233511639363634"));
    powercycling::power_cycle(&device, Duration::from_secs(2))?;
    Ok(())
}
```

For finer control, `PowerPorts::find` returns the ports and `PowerPorts`
switches them:

```rust,no_run
use powercycling::{DeviceId, PowerPorts};
use std::{thread::sleep, time::Duration};

fn main() -> Result<(), powercycling::Error> {
    let ports = PowerPorts::find(&DeviceId::new(0x0483, 0x374e, None))?;
    ports.set_power(false)?; // hold the device off
    sleep(Duration::from_secs(2));
    ports.set_power(true)?;
    Ok(())
}
```

## What gets switched

`PowerPorts::find` walks up from the device to the nearest hub that does
per-port power switching (PPPS) and cuts `PORT_POWER` on its port. A hub without
switches (*ganged*) still *accepts* `PORT_POWER` and answers it by disabling the
port, so the device vanishes from the bus with VBUS untouched — a power cycle
that cuts no power. Skipping those hubs is the point of the walk.

The switched port can be several levels above the device, so the cut takes the
whole branch below it: _all devices_ under the skipped hubs.

## Hubs only, not host controller ports

The walk stops below the root hub, so a device plugged straight into the machine
yields `Error::NoSwitchableHub`. Root hub ports are host controller ports, which
the specification's hub chapter excludes:

> All exposed downstream ports on a USB hub shall support both Enhanced
> SuperSpeed and USB 2.0 connections. **Host controller ports may have different
> requirements.** (§10.1)

Nothing there relates the port numbers of a machine's two root hubs, so the
other half of a root receptacle cannot be identified by the rule below. Put the
device behind a hub that does PPPS.

## Why both halves of a USB 3.x port matter

A USB 3.x receptacle is one physical socket carrying two USB links: a USB 2.0
hub and a `SuperSpeed` hub each own a port on it. A device trains only one of
them and leaves the other reading empty.

The socket has a single VBUS pin, and the two ports feed it like switches wired
in parallel:

> Typically, the only signal shared logic between them is to control VBUS. **If
> either the USB 2.0 hub or Enhanced SuperSpeed hub controllers requires a
> downstream port to be powered, power is turned on for the port.** (§10.1)

Table 10-2 makes it normative: for a hub connected upstream, VBUS reads "On"
wherever one half has `PORT_POWER = 1`, and may only be off when both halves sit
at 0. Cutting only the half your device sits on drops it off the bus while
leaving it powered — the debug session dies, the MCU keeps running. **Both
halves have to be down at once.**

Measured on a Raspberry Pi CM5 with an RTS5411 hub:

| | USB link | VBUS | MCU |
|---|---|---|---|
| cut the device's port only | drops | stays up | keeps running |
| cut both halves | drops | drops | resets |

### Which port is the other half

The kernel names it where it publishes a `peer` link for the port. Where it does
not, the port *number* is known:

> The port numbers assigned to a specific port by the hub shall be consistent
> between the USB 2.0 hub and Enhanced SuperSpeed hub. (§10.3.3)

but the *hub* is not. Nothing relates the port numbers of a machine's two root
hubs (§10.1), and the two buses can be entered at different depths, so the two
halves of one hub need not share a port path. On a CM5 with a USB 2.0-only hub
in front of an RTS5411, the halves of the same chip sit at:

```text
USB 2.0:  2-1 (USB 2.0 only) → 2-1.2 → 2-1.2.3 → device on port 4
SuperSpeed:          usb3 port 2 → 3-2 → 3-2.3 → port 4 empty   ← the peer
```

Same numbers within each pair of halves, different path to them.

So the number is the filter and the hub is left unidentified: every *empty* port
of that number, on an opposite-speed hub that switches power per port, is held
down. The peer is among them by construction, and an empty port feeds nothing,
so the ones that are not the peer cost nothing. Occupied ports are skipped —
re-checked at cut time, not just when the ports were found — so no other device
is disturbed. Root hubs are skipped too: the other half of an external hub's
receptacle is that hub's own other half, never a host controller port.

Nothing is held down when the receptacle has no other half. A half that is not
on the bus holds no `PORT_POWER`, so VBUS follows the half that is (Table 10-2).

Where the kernel *did* name the peer and that port turns out to be occupied, the
link does not mean what this crate takes it to mean: `find` fails with
`Error::PeerNotFound` rather than cutting a port that feeds something else.

## Errors

Nothing succeeds silently. `Error` distinguishes a missing device, a chain with
no switchable hub, a hub that could not be opened, a ganged peer, an
unidentifiable peer, and a device still enumerated after power-off.

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
```

`--verify` samples sysfs while power is off and reports anything that dropped
which should not have. `--primary-only` deliberately skips the other half: if
the LED stays lit, the hold-down is doing real work on your hardware.

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
