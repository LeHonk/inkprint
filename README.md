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
| `epd-inkplate` | Display drivers on `embedded-hal` traits (waveforms TODO) | host + device |
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
cargo run --release --no-default-features -F inkplate10   # another board
```

The firmware has its own toolchain (`channel = "esp"`) and target, so it sits outside
the Cargo workspace and depends on the shared crates by path.
