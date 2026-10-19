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

## Device

```bash
source ~/export-esp.sh
cd firmware
cargo run --release                                   # flash + monitor (Inkplate 6)
cargo run --release --no-default-features -F inkplate6v2  # another board
cargo run --release -F panel-test                     # draw a gray ramp at boot
```

The panel test shows 8 gray bands across the panel with a white square inside the black
band, in the native top-left corner. On an Inkplate 6 held in portrait, white should be at
the top and the square at the bottom left. Without `panel-test` the firmware never touches the panel at boot.

The firmware has its own toolchain (`channel = "esp"`) and target, so it sits outside
the Cargo workspace and depends on the shared crates by path.

## License

MIT OR Apache-2.0, except the display driver ported from Soldered's
[Inkplate-Arduino-library](https://github.com/SolderedElectronics/Inkplate-Arduino-library):
the `epd-inkplate` crate and `firmware/src/panel_bus.rs` are LGPL-3.0-only (see
`epd-inkplate/LICENSE`). Firmware binaries link them statically; since the full source
is published, users can rebuild and relink as the LGPL requires.
