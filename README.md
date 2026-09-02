# powercycling

Power-cycle a USB device identified by VID (Vendor ID), PID (Product ID)
and serial number, by switching hub port power (PPPS). Linux only.

Built for hardware-in-the-loop rigs, where a hanging MCU devboard needs its VBUS
cut rather than only the USB link reset. The crate cuts exactly the one port
that feeds the device, unless explicitly told otherwise.
(For USB 3.x hubs, it _must_ also hold the other half of the receptacle down with it.)

When it cannot be ensured that only the specified device loses power, an error with
precise instructions is returned.

## Requirements

- **Hardware:** a hub that does per-port power switching (PPPS).
  Most bare-board hubs do; many consumer hubs _claim_ it but do not
  have the hardware (MOSFETs) to actually cut the power (you can figure it out
  with the `--verify` command of the CLI).
- **Permissions:** switching a port needs write access to the port's sysfs
  `disable` attribute (kernel 6.0+) or the hub's usbfs node.
  Install uhubctl's [`52-usb.rules`](https://github.com/mvp/uhubctl/blob/master/udev/rules.d/52-usb.rules)
  and re-plug the hub.
- **Identity:** `lsusb` gives VID and PID; the serial is in
  `/sys/bus/usb/devices/<location>/serial`. The serial can be omitted if the
  VID-PID pair is unique on the bus.

## Command line

```text
cargo install powercycling
```

```text
powercycling <vid> <pid> [serial]                 # cycle
powercycling <vid> <pid> [serial] --verify        # check once per setup: LED + collateral damage
powercycling <vid> <pid> [serial] --primary-only  # cut one half only, watch the LED
powercycling <vid> <pid> [serial] --debug         # what the search sees
powercycling --tree                               # the USB tree, hubs with their other half
powercycling --probe <hub> <port>                 # find a missing hub pair by LED
```

Each takes `--pairs-file <path>` for the machine's declared hub pairs; the
first four also `--above <n>` to cycle the hub `n` levels above the device
(e.g. a carrier board) with everything on it.

Run `--verify` once per physical USB-port with a device that has a power LED:
`Ok` alone only proves the device left the bus, not that it lost power.

| LED during `--verify` | meaning |
|---|---|
| dark, and no collateral | power really was cut |
| dark, collateral listed | something else lost power too: the device is behind a hub that does not switch per port |
| stays lit | the port was disabled, VBUS was not: check the hub pairing with `--tree`, `--primary-only` |

Every failure is an error that names the reason and what to do. The
escalation ladder, from "fix the call" to "fix the hardware", is in the
[crate documentation](https://docs.rs/powercycling).

## Library

```rust,no_run
use powercycling::{DeviceId, HubPairs};
use std::time::Duration;

fn main() -> Result<(), powercycling::Error> {
    let device = DeviceId::new(0x0483, 0x374e, Some("0050003A3233511639363634"));
    // Off for 2 s, then up to 10 s for the device to re-enumerate.
    powercycling::power_cycle(
        &device,
        &HubPairs::none(),
        Duration::from_secs(2),
        Duration::from_secs(10),
    )?;
    Ok(())
}
```

`HubPairs` declares which hubs share receptacles where the bus cannot tell,
usually nothing. When, why and how is in the
[crate documentation](https://docs.rs/powercycling), or `cargo doc --open`.

## Credit

The approach, walking to a PPPS-capable hub and switching both halves of a
USB 3.x receptacle, comes from [uhubctl](https://github.com/mvp/uhubctl) by
Vadim Mikhailov, which is where this problem was solved first (to the best
of our knowledge).

## License

MIT or Apache-2.0, at your option.
