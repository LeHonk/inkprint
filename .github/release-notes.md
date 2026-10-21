**Experimental pre-release.** Inkprint turns an Inkplate e-paper board into a network
printer (IPP Everywhere / AirPrint): print a page from a phone or computer and it stays
on the panel.

## Which file?

| Board | ESP32 revision | File | Status |
|---|---|---|---|
| Inkplate 6 (early boards) | v1.0 | `…-inkplate6-rev1-unsafe-…` | Tested. Known instability, see below |
| Inkplate 6 | v3.0 or later | `…-inkplate6-…` | Untested |
| Inkplate 6 v2 | v3.0 or later | `…-inkplate6v2-…` | Untested |

The ESP32 revision is shown when flashing (`Chip is ESP32-D0WDQ6 (revision v1.0)` in
esptool, `Chip revision: v1.0` in espflash). The regular images refuse to boot on a
revision 1 chip.

**rev1-unsafe:** revision 1 chips have a silicon bug in cached PSRAM access that the
Rust compiler does not work around. Expect occasional corrupted pages or crashes. A crash
during a refresh can leave the panel's high-voltage rails on: reset or unplug the board
if it hangs mid-refresh.

For each board there are three files:

- `…-full.bin`: first install. Flash at `0x0`. Erases saved Wi-Fi settings.
- `…-app.bin`: update. Flash at `0x10000`. Keeps saved settings.
- `….elf`: for `espflash`, and for decoding crash backtraces in bug reports.

## Flashing

The easiest way is the [web flasher](https://lehonk.github.io/inkprint/) in desktop
Chrome or Edge. It installs the latest release.

Or with [esptool](https://docs.espressif.com/projects/esptool/) (`pip install esptool`):

```bash
esptool.py --chip esp32 write_flash 0x0 inkprint-<version>-inkplate6-full.bin      # first install
esptool.py --chip esp32 write_flash 0x10000 inkprint-<version>-inkplate6-app.bin   # update
```

Or with [espflash](https://github.com/esp-rs/espflash): `espflash flash --monitor inkprint-<version>-inkplate6.elf`.

On first boot the board opens a Wi-Fi network `Inkprint-xxxx`. Join it with a phone to
pick your network. See the [README](https://github.com/LeHonk/inkprint#wi-fi-setup).

## Reporting problems

Please open an issue with the board, the ESP32 revision, the firmware version (on the
Identify-Printer status screen and at `http://inkplate-xxxx.local:631/`) and the serial
log if you have it.

## Licenses

MIT OR Apache-2.0, except the display driver (`epd-inkplate`, `firmware/src/panel_bus.rs`),
which is LGPL-3.0-only and statically linked. The complete source for this release is
the tagged commit; rebuild instructions are in the README.
