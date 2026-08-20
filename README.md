# powercycling

Power-cycle a USB device identified by `vid:pid:serial`, by switching hub port
power (PPPS). Linux only.

Built for hardware-in-the-loop rigs, where a wedged MCU devboard needs its VBUS
cut rather than its USB link reset.

```rust
use std::time::Duration;

powercycling::power_cycle(0x0483, 0x374e, "0050003A3233511639363634", Duration::from_secs(2))?;
```

For finer control, `PowerPorts::find` returns the ports and `PowerPorts`
switches them:

```rust
let ports = powercycling::PowerPorts::find(vid, pid, serial)?;
ports.set_power(false)?;   // hold the device off
ports.set_power(true)?;
```

## Why both halves of a USB 3.x port matter

A USB 3.x receptacle is one socket carrying two links, exposed as two logical
hubs. A device trains only one of them, but VBUS is gated on `PORT_POWER` of
*both*. Cutting only the half your device sits on drops it off the bus while
leaving it powered — the debug session dies, the MCU keeps running.

Measured on a Raspberry Pi CM5 with an RTS5411 hub:

| | USB link | VBUS | MCU |
|---|---|---|---|
| cut the device's port only | drops | stays up | keeps running |
| cut both halves | drops | drops | resets |

This crate finds the other half without identifying the hub it belongs to. One
receptacle holds one device, so the peer of an occupied port is necessarily
empty; holding down every empty opposite-speed port includes it, and skipping
occupied ports leaves other devices untouched. Where the kernel publishes a
`peer` link, it names the port exactly and only one is held.

The search also walks up past hubs that report ganged switching, since clearing
`PORT_POWER` there disconnects a port without dropping VBUS.

## Errors

Nothing succeeds silently. `Error` distinguishes a missing device, a chain with
no switchable hub, a hub that could not be opened, a ganged peer, and a device
still enumerated after power-off.

The one case it cannot detect: a hub that reports per-port power switching, cuts
the link, and keeps VBUS up anyway. Confirm with an LED or a meter once per
hardware setup — see `--primary-only` below.

## Permissions

Switching a port needs write access to either the port's sysfs `disable`
attribute (kernel 6.0+) or the hub's usbfs node. Use uhubctl's
[`52-usb.rules`](https://github.com/mvp/uhubctl/blob/master/udev/rules.d/52-usb.rules).
A missing rule surfaces as `Error::HubUnreadable`.

## Troubleshooting

```
cargo run --example cycle -- <vid> <pid> <serial>                 # cycle
cargo run --example cycle -- <vid> <pid> <serial> --debug         # what the search sees
cargo run --example cycle -- <vid> <pid> <serial> --verify        # check for collateral damage
cargo run --example cycle -- <vid> <pid> <serial> --primary-only  # cut one half, watch the LED
```

`--verify` samples sysfs while power is off and reports anything that dropped
which should not have. `--primary-only` deliberately skips the other half: if
the LED stays lit, the hold-down is doing real work on your hardware.

## Credit

The approach — walking to a PPPS-capable hub, and the need to switch both halves
of a USB 3.x receptacle — comes from [uhubctl](https://github.com/mvp/uhubctl)
by Vadim Mikhailov, which is where this problem was solved first.

## License

MIT or Apache-2.0, at your option.
