# Inkprint: IPP print server for Inkplate 6 (Rust)

## User story

A user prints something, such as a cooking recipe, from a phone or computer, and the
Inkplate shows the last page printed. The screen never clears, so the recipe stays
readable while cooking without touching anything with sticky fingers. A phone or tablet,
by contrast, turns its screen off after a period of inactivity.

The firmware is configurable at compile time for other Inkplate variants (larger screens,
color panels). The first iteration targets the black-and-white/grayscale Inkplate 6.

## Status (2026-10-20)

The user story works end to end on an Inkplate 6 (original, ESP32 rev 1.0, no RTC): print
a recipe from Chrome on Android and the panel keeps showing the last page. Milestones 1–7
are done; milestone 8 (other boards) waits for hardware. The sections below this one are
the original plan, kept for its reasoning; where the implementation went differently, the
list further down says so.

**Verified on the Inkplate 6:** 3-bit gray display, Wi-Fi, mDNS (`avahi-browse`), IPP
(`ipptool`), printing from Chrome on Android (page order, last page shown), Identify-Printer
status screen with ghost-free restore, Wi-Fi setup portal and settings in flash, NTP time,
JPEG photos (largest 7.5 MB, about a minute), brightness setting, panel init (10 of 10
boots after the I2C fix).

**Verified on the host only:** CUPS `ipp-everywhere.test` (26 pass, 0 fail, 13 skipped for
optional features), 92 unit and integration tests, the PCF85063A RTC driver (against a
register-level mock), Inkplate 6 V2 / 10 / 6PLUS / 5 / 6COLOR firmware builds.

**Not tested yet:** iOS / macOS (AirPrint, the printer icon), Windows, Android defaulting to
4×6 paper after the ready-media change, any board other than the original Inkplate 6.

### Where the implementation differs from the plan

- **Display data path:** the panel is fed by I2S1 in LCD mode with DMA, as the current
  Arduino library does, not by bit-banged GPIO. The driver (`epd-inkplate`) is a port of
  that library and therefore LGPL-3.0; the rest is MIT OR Apache-2.0.
- **JPEG is supported** after all: a streaming baseline decoder (`crates/jpeg`) decodes one
  row of 8×8 blocks at a time, so phone photos fit in memory. Progressive JPEGs are
  rejected. IPP Everywhere requires JPEG, and with it `ipp-everywhere.test` passes.
- **All IPP Everywhere operations** are implemented (Create-Job, Send-Document, Cancel-Job,
  Close-Job, Cancel-My-Jobs, Identify-Printer), not only Print-Job.
- **DNS-SD records are our own** (`crates/dnssd`): edge-mdns publishes subtypes as a
  two-level PTR chain that `_print` / `_universal` browsers cannot resolve.
- **Android quirks:** its print service has no A6, so 4×6 is reported as ready next to A6;
  it sends pages in reverse for face-up output bins, so the printer reports face-down.
- **Settings and setup:** Wi-Fi network, password, location and brightness live in the
  `nvs` partition, set through a captive portal (`Inkprint-xxxx` access point) or
  `/setup` on the LAN. Compiled-in credentials remain a development fallback.
- **Time:** NTP from `pool.ntp.org` daily, plus the PCF85063A RTC on boards that have one.
  The NTP server from the DHCP lease (option 42) is not used: embassy-net 0.9 doesn't
  expose DHCP options.
- **I2C on the original ESP32:** the transfer after an expected NACK sometimes timed out,
  depending on code layout. Devices are therefore only probed when the boot scan saw them;
  `INKPRINT_I2C_DIAG=1` replays the failing pattern. Cause not pinned down: esp-hal's NACK
  recovery on this chip, or the rev 1 PSRAM bug.
- **Rev 1.0 ESP32:** esp-hal requires rev 3; the override lives in the git-ignored
  `firmware/.env` (see README) because Rust code doesn't work around the PSRAM cache bug.

### Remaining work

- **Crop tuning,** once real recipes show what needs changing.
- **Other boards (milestone 8):** Inkplate 6 V2, 10, 6PLUS, 5, then the 6COLOR color path.
  Their `Board` entries exist; panel drivers, waveforms and pin maps need porting and
  hardware to test on.
- **Client coverage:** test iOS / macOS (AirPrint, icon) and Windows. If current iOS no
  longer prints to plain `ipp://`, `ipps` (TLS) becomes necessary.
- **Optional:**
  - NTP server from DHCP (needs an embassy-net change);
  - faster large JPEGs (scaled IDCT);
  - per-job contrast from IPP (`print-darkness`, `print-content-optimize`);
  - touchpad controls on boards that have them;
  - a password for the setup page and access point.

## 0. Decisions that shape the rest

**1. Do not render PDF on the device. Accept only raster formats.**
The phone or computer already turns the document into pixels. If the printer advertises
only `image/pwg-raster` (IPP Everywhere, used by CUPS, Android and Windows) and
`image/urf` (Apple Raster, used by iOS and macOS AirPrint), every client sends
ready-made bitmaps. Both formats are simple run-length-compressed scanlines, so they can
be decoded as a stream, one line at a time, straight into a framebuffer. This is the
only approach that fits an ESP32 comfortably.

**2. Advertise small paper sizes so the text stays readable.**
An A4 page shrunk onto a 6″ screen (about 122×91 mm) makes 11 pt text about 5 pt, which
is too small to read while cooking. So:

- Advertise A6 and 4×6″ index card as the **default** paper sizes. Their shape is close
  to the portrait screen, and the print dialog will lay the content out larger.
- Still accept A4 and Letter for compatibility, and **auto-crop the whitespace margins**
  before scaling. A recipe printed on A4 then gets enlarged to fill the screen.
- Page selection happens on the client. The user picks the page in the print dialog,
  and the device shows the last page it receives.

**3. Use the bare-metal Rust stack (`esp-hal` + Embassy), not `esp-idf-svc`.**
As of 2025–26, Espressif's official Rust focus is `esp-hal` (1.0 released Oct 2025).
The `esp-idf-*` crates are community-maintained and moving more slowly. One more reason:
ESP-IDF's built-in HTTP server (`esp_http_server`) cannot decode chunked request bodies.
CUPS sends those routinely, so a custom HTTP layer is needed either way.

The `std` + ESP-IDF route still works (mature lwIP and mDNS) and is the fallback if the
bare-metal Wi-Fi or mDNS stack causes trouble. Keep that option open by putting all
protocol logic in `no_std` crates that don't depend on either stack.

## 1. Tooling to install

```bash
# Rust + the Espressif Xtensa toolchain (the ESP32 is Xtensa LX6, not RISC-V)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo install espup --locked
espup install                     # installs the "esp" toolchain, Xtensa LLVM fork, GCC
source ~/export-esp.sh            # add to your shell rc

cargo install espflash --locked   # flash + serial monitor (espflash 4.x)
cargo install esp-generate --locked   # project template generator
# esp-generate --chip esp32 inkprint   -> enable: alloc, wifi, embassy, psram

# Linux serial access (Inkplate 6 uses a CH340 USB-UART; ch341 module is in-kernel)
sudo usermod -aG dialout $USER

# Host-side testing tools (Debian)
sudo apt install cups-ipp-utils avahi-utils cups
#   ipptool        – runs IPP conformance tests (ipp-everywhere.test etc.)
#   ippfind        – discovers printers
#   ippeveprinter  – reference IPP Everywhere printer, to diff attributes against
#   avahi-browse -rt _ipp._tcp  – check the mDNS advertisement
```

**Targets:**

- Bare-metal: `xtensa-esp32-none-elf` with toolchain channel `esp`.
- If falling back to std: `xtensa-esp32-espidf`, plus `cargo install ldproxy`, and
  `python3`, `cmake`, `ninja` installed. `embuild` downloads ESP-IDF itself.
- Some newer Inkplate models use an ESP32-S3 (`xtensa-esp32s3-none-elf`). `espup`
  covers that too.

## 2. Useful crates

Check current versions on crates.io; the `esp-*` ecosystem moves quickly.

| Area | Crate(s) | Notes |
|---|---|---|
| HAL | `esp-hal` 1.x | GPIO, I2C, direct register access for fast parallel display writes, PSRAM support |
| Async runtime | `embassy-executor`, `embassy-time`, `embassy-sync`, `esp-rtos` | `esp-rtos` replaced `esp-hal-embassy` in the 1.0 release. Check the versions `esp-generate` pins. |
| Wi-Fi | `esp-radio` (formerly `esp-wifi`) | STA mode; credentials via `env!()` at compile time in v1 |
| TCP/IP | `embassy-net` (smoltcp) | Needs UDP multicast for mDNS |
| HTTP | `edge-http` + `edge-nal-embassy` | Handles chunked request bodies. Must handle `Expect: 100-continue` (CUPS sends it). `picoserve` is an alternative, but check its chunked-request support. |
| mDNS / DNS-SD | `edge-mdns` | Publishes `_ipp._tcp` with subtypes and TXT records |
| Memory | `esp-alloc` | Heap in internal RAM plus a PSRAM region for framebuffers |
| Boot/debug | `esp-bootloader-esp-idf`, `esp-println`, `esp-backtrace`, `defmt` (optional) | |
| IPP codec | **Write our own** (about 500 lines) | `ipp` (ipp.rs) is client-oriented and requires std. `ippper` is a std/tokio IPP *server* library. Both are good references for the attribute set and for the host simulator, but neither runs on bare metal. |
| Raster decode | **Write our own** | PWG Raster ("RaS2", 1796-byte page header) and URF ("UNIRAST", 32-byte page header) use the same PackBits-like line encoding. Write it as a push-based state machine. |
| IO expander | `port-expander` | Supports the MCP23017 (Inkplate 6 v1) and the PCAL6416A (newer revisions) |
| PMIC | Write our own (small) | TPS65186 over I2C: power-up sequence, VCOM, temperature |
| EPD driver | **Port it ourselves** | No known maintained Rust Inkplate driver. Port from Soldered's `Inkplate-Arduino-library` (Inkplate6 driver + waveform tables). turgu1/ESP-IDF-InkPlate (C++) is a cleaner second reference. |
| Graphics | `embedded-graphics`, `u8g2-fonts` | Status screens only (IP address, "ready", errors) |
| Simulator | `embedded-graphics-simulator`, `tokio`, `image` | For the host build |
| JPEG (later) | `zune-jpeg` (no_std + alloc) | Optional. It decodes the whole image, so large photos need a size limit. |

## 3. Implementation outline

### Workspace layout

```
inkprint/
├── Cargo.toml                # workspace for host-testable crates
├── crates/
│   ├── ipp-codec/            # no_std+alloc: IPP message parse/encode, typed attributes
│   ├── ipp-printer/          # no_std: operations, job table, attribute catalog built from Board caps
│   ├── raster/               # no_std: streaming PWG + URF decoders → per-line callback
│   ├── render/               # no_std: crop, scale, rotate, dither → PageBuffer
│   └── board/                # no_std: Board trait + capability consts per Inkplate variant
├── epd-inkplate/             # no_std: display drivers (parallel-EPD family, SPI-color family)
├── firmware/                 # esp32 binary; own rust-toolchain.toml (channel = "esp")
│   └── .cargo/config.toml    #   and target; excluded from workspace
└── sim/                      # host binary: same IPP server on tokio, renders to window/PNG
```

The simulator is the biggest productivity gain. `ipptool` and real phones can print
against a laptop, with second-long rebuilds, before the display driver even works. For
simulator discovery, `avahi-publish-service` is enough.

### Data path (streaming, nothing buffered whole)

```
TCP → HTTP (de-chunk, 100-continue) → IPP header parser (attributes, cap ~16 KB)
    → document bytes → raster decoder (push state machine, arbitrary chunk sizes)
    → on_line(y, &[u8]) → render: bbox/crop pass → area-average downscale → rotate → PageBuffer
    → on_page_end: swap back/candidate buffer
    → on_document_end: candidate = last page → display full refresh
```

- **Last-page logic:** a page is only known to be the last one when the stream ends.
  Keep two page buffers in PSRAM: the page being written and the last completed page.
  600×800 at 8-bit gray is 480 KB each, so both fit easily in the 4 MB of directly
  mapped PSRAM.
- **Auto-crop:** finding the content bounding box needs either the whole page or two
  passes. Store the page at reduced resolution (for example, received at 150 dpi on A6
  is about 620×875 at 8 bpp, roughly 540 KB), then crop and scale to the panel at page
  end. That is simpler than doing it as a stream.
- **Output:** 3-bit grayscale (8 levels, which the Inkplate 6 supports) or 1-bit with
  Floyd–Steinberg dithering. Make this a compile-time choice. Text looks best in
  grayscale with light sharpening.

### IPP surface (no TLS, port 631, resource `/ipp/print`)

- **Operations:** Print-Job, Validate-Job, Create-Job, Send-Document,
  Get-Job-Attributes, Get-Jobs, Cancel-Job, Cancel-My-Jobs, Close-Job,
  Get-Printer-Attributes, Identify-Printer (flash an "I'm here" banner).
- **Jobs:** one at a time. Process the job inside the request, then report it as
  completed. Keep a ring buffer of about 10 finished jobs for Get-Jobs. If a second job
  arrives during processing, return `server-error-busy`.
- **Key printer attributes, all generated from `Board` consts:**
  - `document-format-supported` = octet-stream, image/pwg-raster, image/urf
  - `pwg-raster-document-type-supported` = sgray_8 (later srgb_8 for color)
  - `pwg-raster-document-resolution-supported` and `urf-supported`
    (e.g. `W8,SRGB24,CP1,RS150-300`)
  - `media-supported` / `media-col-database` (A6 default, 4×6, A5, A4, Letter),
    all margins 0 (borderless)
  - `print-color-mode-supported`, `sides-supported` = one-sided, `copies-supported` = 1–1
  - `printer-kind` = document, `ipp-features-supported` = ipp-everywhere, `printer-uuid`
  - `printer-icons`: serve a PNG over HTTP; macOS wants it
- **mDNS:** register `_ipp._tcp` with subtypes `_print._sub` (IPP Everywhere) and
  `_universal._sub` (AirPrint). TXT records:
  `txtvers=1 qtotal=1 rp=ipp/print ty=… pdl=image/pwg-raster,image/urf URF=… Color=F Duplex=F UUID=… kind=document`.

### Display / "never clears"

- **At boot, never touch the panel** if it already shows content. Show the status/IP
  screen only on first boot or if Wi-Fi fails. The e-ink keeps the image without power,
  so a reboot loses nothing.
- When a new page arrives, do a full refresh with clean cycles to avoid ghosting.
- Optionally save the last page to a flash partition (about 60–240 KB). That allows
  partial-update overlays later, such as a tiny Wi-Fi/battery indicator, without
  redrawing everything.
- Power: Wi-Fi has to stay on, so plan for USB power in the kitchen. Use modem sleep to
  reduce idle current.

### Compile-time board selection

Cargo features `inkplate6` (default), `inkplate6v2`, `inkplate10`, `inkplate6plus`,
`inkplate5`, `inkplate6color`, etc., each selecting one `impl Board`:

```rust
pub trait Board {
    const WIDTH: u16; const HEIGHT: u16;          // native panel orientation
    const ROTATION: Rotation;                     // portrait for recipes
    const PIXEL: PixelKind;                       // Mono1, Gray3, Color7
    const DPI_HINT: u16;                          // drives advertised resolution / media
    type Display: EpdDriver;                      // ParallelEpd<Waveform> or SpiColorEpd
}
```

There are two driver families:

- **Parallel EPD** (6, 10, 6PLUS, 5): bit-banged 8-bit bus plus waveforms. This is
  where the porting work is.
- **SPI controller** (6COLOR, ACeP 7-color): much simpler to drive. It needs color
  dithering to a 7-color palette, and the advertised formats switch to srgb_8 with
  `Color=T`.

## 4. Milestones

Status markers: ✅ done, ◐ partly done, ⏳ waiting.

1. ✅ **Bring-up:** `esp-generate` project, serial log, PSRAM allocation verified, I2C scan
   finds the expander and PMIC.
2. ✅ **Display:** PMIC power sequence, 1-bit full refresh of a test pattern, then 3-bit
   gray. *(Highest risk. Do it early.)*
3. ◐ **Codecs on host:** IPP codec and raster decoders with golden tests. Capture real jobs
   from CUPS, Android and iOS using the simulator. *Done with synthetic rasters, an
   independent JPEG encoder and real camera photos; no captured Android/iOS jobs.*
4. ✅ **Simulator end-to-end:** print from laptop and phone to `sim`, with the result
   rendered as a PNG. Pass `ipptool -tv ipp://host/ipp/print ipp-everywhere.test` as far
   as is reasonable. *`ipp-everywhere.test` passes fully; phones were tested against the
   device rather than the simulator.*
5. ✅ **Network on device:** Wi-Fi, mDNS visible in `avahi-browse`, Get-Printer-Attributes
   over HTTP.
6. ✅ **Integration:** streaming Print-Job into PSRAM and showing the last page on the panel.
7. ◐ **Polish:** Identify-Printer, printer icon, status screen, crop tuning, Wi-Fi
   provisioning (captive portal instead of compile-time credentials). *All done except
   crop tuning; also added: location and brightness settings, NTP/RTC time.*
8. ⏳ **Other boards:** Inkplate 10 / 6PLUS features, then the 6COLOR color path. *Waiting
   for hardware.*

## 5. Risks to check early

*Outcomes so far:* display timing worked by porting upstream's I2S path; mDNS on bare metal
works (with our own DNS-SD records); Android and CUPS print fine; iOS and Windows are
untested; the board turned out to be a rev 1.0 ESP32 — no PSRAM artifacts seen in
printing, but see the I2C note in Status.

- **Display waveforms and timing:** these are panel-specific, and timing-critical GPIO
  access from PSRAM-backed buffers is a concern. The Arduino library proves the hardware
  can do it, but expect iteration.
- **Client compatibility:**
  - iOS: test as early as possible whether current iOS still discovers plain `ipp://`
    (non-TLS) printers. If not, `ipps` via `esp-mbedtls` or `embedded-tls` is needed,
    which is a sizeable addition.
  - Android: the built-in Default Print Service supports pwg-raster.
  - Windows: the IPP class driver works with pwg-raster.
- **mDNS stack maturity on bare metal.** The fallback is the ESP-IDF mdns component via
  `esp-idf-svc`.
- **ESP32 silicon revision:** rev 1 chips have a PSRAM cache bug. Inkplates normally ship
  with rev 3 (D0WD-V3), but check what the boot log reports.
