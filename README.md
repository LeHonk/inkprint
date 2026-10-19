# Inkprint

An IPP Everywhere / AirPrint print server for the Inkplate e-paper boards, in Rust.
Print a recipe from your phone and the panel keeps showing the last page. See
[PLAN.md](PLAN.md) for the design and milestones.

## Layout

| Path | What | Builds on |
|---|---|---|
| `crates/board` | Per-board capabilities, selected with a Cargo feature | host + device |
| `crates/ipp-codec` | IPP message parser/encoder | host + device |
| `crates/ipp-printer` | Sans-IO printer: operations, jobs, attributes, mDNS TXT | host + device |
| `crates/raster` | Streaming PWG Raster / URF decoder | host + device |
| `crates/render` | Last-page capture, auto-crop, fit, rotate, quantize | host + device |
| `crates/ipp-http` | IPP over HTTP: edge-http handler for the printer | host + device |
| `crates/dnssd` | DNS-SD records (`_ipp._tcp`, `_print`/`_universal` subtypes) for edge-mdns | host + device |
| `epd-inkplate` | Inkplate 6 / 6 V2 panel driver, PMIC, IO expanders (ported, LGPL-3.0) | host + device |
| `sim` | Host simulator: the printer over hyper, writes `last-page.png` | host |
| `firmware` | ESP32 binary (`esp-hal`, Embassy, `esp-radio`) | device only |

## Host: tests and simulator

```bash
cargo test --workspace
cargo run -p sim                    # listens on :6310, prints an avahi-publish-service line
```

Run the printed `avahi-publish-service` command in another terminal and the simulator
shows up as a printer on phones and computers on the LAN. Or test it directly:

```bash
python3 tools/make_pwg.py recipe.pwg
ipptool -tv -f recipe.pwg -d filetype=image/pwg-raster ipp://localhost:6310/ipp/print print-job.test
ipptool -t ipp://localhost:6310/ipp/print get-printer-attributes.test
```

`cargo run -p ipp-http --example serve -- 6311` serves the same IPP handler the firmware
uses (edge-http), which is handy for `ipptool` runs against the device code path.

## Device

Wi-Fi credentials are compiled in from the environment. Put them in the git-ignored
`firmware/.env` and source it before building:

```bash
export WIFI_SSID='my network'
export WIFI_PASSWORD='secret'      # empty for an open network
```

```bash
source ~/export-esp.sh
cd firmware
source .env
cargo run --release                                   # flash + monitor (Inkplate 6)
cargo run --release --no-default-features -F inkplate6v2  # another board
cargo run --release -F panel-test                     # draw a gray ramp at boot
```

The panel test shows 8 gray bands across the panel with a white square inside the black
band, in the native top-left corner. On an Inkplate 6 held in portrait, white should be at
the top and the square at the bottom left. Without `panel-test` the firmware never touches the panel at boot.

Once connected, the log shows the IP address and the printer URI. The printer is
advertised over mDNS as `Inkplate 6 (xxxx)` on `inkplate-xxxx.local`, where `xxxx` comes
from the MAC address. Check from a Linux machine on the same network:

```bash
avahi-browse -rt _ipp._tcp
ipptool -tv ipp://inkplate-xxxx.local:631/ipp/print get-printer-attributes.test
```

The firmware has its own toolchain (`channel = "esp"`) and target, so it sits outside
the Cargo workspace and depends on the shared crates by path.

### ESP32 revision 1 boards

esp-hal requires ESP32 chip revision 3.0 or later, and the boot log (`boot: chip
revision: v1.0`) or espflash will complain on older chips, as found on early Inkplate 6
boards. Revisions below 3.0 have a silicon bug in cached PSRAM access that ESP-IDF's C
compiler works around (`-mfix-esp32-psram-cache-issue`) but Rust does not. The firmware
keeps its page buffers in PSRAM, so on these chips expect occasional data corruption or
crashes. A crash during a refresh can leave the panel's high-voltage rails on; reset or
unplug the board if it hangs mid-refresh.

The override is therefore never committed. To accept the risk on your own machine, create
`firmware/.env` (ignored by git) and source it before building:

```bash
echo 'export ESP_HAL_CONFIG_MIN_CHIP_REVISION=100' > firmware/.env
cd firmware && source .env && cargo run --release
```

The setting is read at build time, so it only takes effect for builds run from that
shell. Boards with revision 3.0 or later (ESP32-WROVER-E) need nothing.

## License

MIT OR Apache-2.0, except the display driver ported from Soldered's
[Inkplate-Arduino-library](https://github.com/SolderedElectronics/Inkplate-Arduino-library):
the `epd-inkplate` crate and `firmware/src/panel_bus.rs` are LGPL-3.0-only (see
`epd-inkplate/LICENSE`). Firmware binaries link them statically; since the full source
is published, users can rebuild and relink as the LGPL requires.
