//! Decodes JPEGs made by an independent encoder and compares with the source pixels and
//! with the `image` crate's decoder.

use jpeg::{Error, Orientation, decode};
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use raster::{PageInfo, RasterSink};

#[derive(Default)]
struct Page {
    info: Option<PageInfo>,
    lines: Vec<Vec<u8>>,
    ended: bool,
}

impl RasterSink for Page {
    fn begin_page(&mut self, info: &PageInfo) {
        self.info = Some(*info);
    }
    fn line(&mut self, y: u32, data: &[u8]) {
        assert_eq!(y as usize, self.lines.len());
        self.lines.push(data.to_vec());
    }
    fn end_page(&mut self) {
        self.ended = true;
    }
}

/// Delivers the input in fixed-size chunks to exercise every buffer boundary.
struct Chunked<'a> {
    data: &'a [u8],
    chunk: usize,
}

impl embedded_io_async::ErrorType for Chunked<'_> {
    type Error = core::convert::Infallible;
}

impl embedded_io_async::Read for Chunked<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let n = self.chunk.min(buf.len()).min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

fn run(jpeg: &[u8], chunk: usize) -> (Result<jpeg::Decoded, Error>, Page) {
    let mut page = Page::default();
    // The caller usually has some bytes already (after the IPP header): pass the first 5.
    let split = 5.min(jpeg.len());
    let mut rest = Chunked { data: &jpeg[split..], chunk };
    let result = embassy_futures::block_on(decode(&jpeg[..split], &mut rest, &mut page));
    (result, page)
}

/// A test card: gradients, hard edges and fine lines, like a photographed recipe.
fn source(width: usize, height: usize) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            let text = (y / 6) % 3 == 0 && (x / 4) % 5 != 0 && x > width / 8 && x < width * 7 / 8;
            let (r, g, b) = if text {
                (20, 20, 30)
            } else {
                ((x * 255 / width) as u8, (y * 255 / height) as u8, ((x + y) % 256) as u8)
            };
            rgb.extend([r, g, b]);
        }
    }
    rgb
}

fn luma(rgb: &[u8]) -> Vec<u8> {
    rgb.chunks(3)
        .map(|p| (0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2])).round() as u8)
        .collect()
}

fn encode(data: &[u8], w: u16, h: u16, color: ColorType, configure: impl FnOnce(&mut Encoder<&mut Vec<u8>>)) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = Encoder::new(&mut out, 95);
    configure(&mut enc);
    enc.encode(data, w, h, color).unwrap();
    out
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| f64::from(x.abs_diff(*y))).sum::<f64>() / a.len() as f64
}

/// Our gray output vs. the `image` crate decoding the same file to RGB, then to JPEG luma.
fn reference_luma(jpeg: &[u8]) -> Vec<u8> {
    let img = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).unwrap();
    luma(img.to_rgb8().as_raw())
}

fn check(jpeg: &[u8], w: usize, h: usize, original_luma: Option<&[u8]>, tolerance: f64) {
    check_chunks(jpeg, w, h, original_luma, tolerance, &[1, 7, 4096]);
}

fn check_chunks(jpeg: &[u8], w: usize, h: usize, original_luma: Option<&[u8]>, tolerance: f64, chunks: &[usize]) {
    for &chunk in chunks {
        let (result, page) = run(jpeg, chunk);
        let decoded = result.unwrap_or_else(|e| panic!("chunk {chunk}: {e:?}"));
        assert_eq!((decoded.width as usize, decoded.height as usize), (w, h));
        assert!(page.ended);
        let info = page.info.unwrap();
        assert_eq!((info.width as usize, info.height as usize, info.bits_per_pixel), (w, h, 8));
        assert_eq!(page.lines.len(), h);
        let ours: Vec<u8> = page.lines.concat();
        let reference = mean_abs_diff(&ours, &reference_luma(jpeg));
        assert!(reference < tolerance, "chunk {chunk}: vs image crate {reference:.2}");
        if let Some(orig) = original_luma {
            let lossy = mean_abs_diff(&ours, orig);
            assert!(lossy < 3.0, "chunk {chunk}: vs source {lossy:.2}");
        }
    }
}

#[test]
fn grayscale() {
    let (w, h) = (333, 217); // not multiples of 8 or 16
    let gray = luma(&source(w, h));
    let jpeg = encode(&gray, w as u16, h as u16, ColorType::Luma, |_| {});
    check(&jpeg, w, h, Some(&gray), 1.0);
}

#[test]
fn color_subsamplings() {
    let (w, h) = (333, 217);
    let rgb = source(w, h);
    for sampling in [SamplingFactor::R_4_4_4, SamplingFactor::R_4_2_2, SamplingFactor::R_4_2_0, SamplingFactor::F_1_2] {
        let jpeg = encode(&rgb, w as u16, h as u16, ColorType::Rgb, |e| e.set_sampling_factor(sampling));
        // Chroma rounding in the reference's RGB round trip costs a little.
        check(&jpeg, w, h, Some(&luma(&rgb)), 2.0);
    }
}

#[test]
fn restart_markers() {
    let (w, h) = (200, 120);
    let rgb = source(w, h);
    for interval in [1, 3, 7] {
        let jpeg = encode(&rgb, w as u16, h as u16, ColorType::Rgb, |e| e.set_restart_interval(interval));
        assert!(jpeg.windows(2).any(|p| p == [0xFF, 0xD0]), "encoder wrote no RST markers");
        check(&jpeg, w, h, Some(&luma(&rgb)), 2.0);
    }
}

#[test]
fn exif_orientation() {
    let (w, h) = (64, 48);
    let gray = luma(&source(w, h));
    let mut exif = b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01".to_vec();
    exif.extend([0x01, 0x12, 0x00, 0x03, 0, 0, 0, 1, 0, 6, 0, 0]); // orientation 6
    let jpeg = encode(&gray, w as u16, h as u16, ColorType::Luma, |e| e.add_app_segment(1, exif).unwrap());
    let (result, _) = run(&jpeg, 64);
    assert_eq!(result.unwrap().orientation, Orientation::Rotate90);
}

#[test]
fn rejects_progressive_and_garbage() {
    let rgb = source(64, 48);
    let progressive = encode(&rgb, 64, 48, ColorType::Rgb, |e| e.set_progressive(true));
    assert_eq!(run(&progressive, 64).0, Err(Error::Unsupported("progressive JPEG")));
    assert_eq!(run(b"%PDF-1.7 not a jpeg", 64).0, Err(Error::NotJpeg));
    let full = encode(&rgb, 64, 48, ColorType::Rgb, |_| {});
    let (result, page) = run(&full[..full.len() / 2], 64);
    assert!(matches!(result, Err(Error::Malformed(_))), "{result:?}");
    assert!(!page.ended);
}

/// JPEGs made by other software: the directory in `JPEG_SAMPLES` (e.g. camera photos),
/// else the system's pixmaps. Run with `--release` for large photos.
#[test]
fn real_world_files() {
    let dir = std::env::var("JPEG_SAMPLES").unwrap_or_else(|_| "/usr/share/pixmaps/faces".into());
    let Ok(dir) = std::fs::read_dir(&dir) else { return };
    let mut checked = 0;
    for entry in dir.flatten() {
        let path = entry.path();
        let is_jpeg = path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg"));
        if !is_jpeg || checked >= 8 {
            continue;
        }
        let data = std::fs::read(&path).unwrap();
        let img = image::load_from_memory_with_format(&data, image::ImageFormat::Jpeg).unwrap();
        match run(&data, 4096).0 {
            Ok(d) => {
                assert_eq!((d.width as u32, d.height as u32), (img.width(), img.height()), "{path:?}");
                check_chunks(&data, img.width() as usize, img.height() as usize, None, 2.0, &[4096]);
                eprintln!("{}: {}x{}, {:?}", path.file_name().unwrap().to_string_lossy(), d.width, d.height, d.orientation);
                checked += 1;
            }
            Err(Error::Unsupported(why)) => eprintln!("{path:?}: unsupported ({why})"),
            Err(e) => panic!("{path:?}: {e:?}"),
        }
    }
    eprintln!("checked {checked} real-world JPEGs");
}
