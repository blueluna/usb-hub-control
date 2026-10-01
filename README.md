# usb-hub-control

Control port power on USB hubs that support individual port power switching.

## USB 3 hubs

A USB 3 hub shows up as two hubs, a USB 2 hub and a SuperSpeed hub, usually on different buses.
Every physical connector has a port on both, and VBUS is only cut when both ports are switched
off.

`uhubctrl` handles this by fusing the two halves. `list` shows them together, and `power` and
`cycle` switch the peer port on the other half too. On Linux the peer ports come from the
kernel's port peer links in sysfs, otherwise from hubs with the same container id. Pass
`--single` to only switch the given hub.

## Listing hubs

```shell
$ cargo run --release -- list

...

5-1 0bda:5411 09 00 02 0004 4 e5cdb920397011e0a9350002a5d5c51b + 6-1 0bda:0411 09 00 03 0004 4 e5cdb920397011e0a9350002a5d5c51b
 1 0100 powered / 82a0 powered
 2 0100 powered / 82a0 powered
 3 0503 connection enabled powered / 8203 connection enabled powered 5-1.3 0bda:5411 09 00 02 0004 4 e5cdb920397011e0a9350002a5d5c51b + 6-1.3 0bda:0411 09 00 03 0004 4 e5cdb920397011e0a9350002a5d5c51b
   1 0100 powered / 8203 connection enabled powered 006:005 05e3:0749 Generic USB3.0 Card Reader 000000001532
   2 0103 connection enabled powered / 82a0 powered 005:005 045e:0823 Microsoft Microsoft® Classic IntelliMouse®
   3 0103 connection enabled powered / 82a0 powered 005:008 046d:c345 Logitech Logi TKL Mechanical Keyboard
   4 0103 connection enabled powered / 82a0 powered 005:036 c0de:cafe rp2350-playground USB serial display 00000001

...

```

Each port line shows the USB 2 port status, then the SuperSpeed peer port status after `/`, then
the devices connected to either. Hubs without a SuperSpeed half are listed as before.

The location of the RP2350 above is 5-1.3 and the port is 4. The USB 3 location, 6-1.3, can be
used as well.

## Finding a device

`find` prints the hub location and port of devices matching a `vid:pid`, or text in the
manufacturer, product or serial string,

```shell
$ cargo run --release -- find c0de:cafe
-l 5-1.3 -p 4  005:044 c0de:cafe rp2350-playground USB serial display 00000001
```

A board that is powered off, or runs firmware without USB, can not be found. Find it while it
runs firmware with USB, note the location and port, and use them from then on. They stay the
same as long as the hubs are plugged into the same ports.

## Switching power

To power off that port, on both halves,

```shell
cargo run --release -- power --location 5-1.3 --port 4
```

To power on that port,

```shell
cargo run --release -- power --location 5-1.3 --port 4 --on
```

To power cycle that port, keeping it off for 2 seconds,

```shell
cargo run --release -- cycle --location 5-1.3 --port 4 --delay 2000
```

When switching off, the USB 2 half is switched first. When switching on, the USB 2 half is
switched last.
