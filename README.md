# usb-hub-control

```shell
$ cargo run --release -- list

...

1-2 2109:2817 09 00 02 0364 4 03071002060000000a1003000e000104
 1 0100 powered
 2 0100 powered
 3 0100 powered
 4 0503 connection enabled powered 1-2.4 2109:2817 09 00 02 9023 4 03071002060000000a1003000e000104
   1 0100 powered
   2 0103 connection enabled powered 001:021 10c4:ea60 Silicon Labs CP2102N USB to UART Bridge Controller aacd7a748a71f0119e80029f1045c30f
   3 0100 powered
   4 0503 connection enabled powered 1-2.4.4 2109:2817 09 00 02 9023 4 03071002060000000a1003000e000104
     1 0100 powered
     2 0100 powered
     3 0103 connection enabled powered 001:016 2e8a:000c Raspberry Pi Debug Probe (CMSIS-DAP) E6633861A36F5238
     4 0100 powered

...

```

The location of the aspberry Pi Debug Probe above is, 1-2.4.4 and the port is 3.

To power on that port,

```shell
cargo run -- power --location 1-2.4.4 --port 3 --on
```

To power off that port,

```shell
cargo run -- power --location 1-2.4.4 --port 3
```
