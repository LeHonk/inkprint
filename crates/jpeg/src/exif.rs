//! EXIF orientation (TIFF tag 0x0112 in IFD0 of the APP1 "Exif" segment).

use crate::Orientation;

/// Orientation from an APP1 payload (starting with "Exif\0\0"). Mirrored orientations
/// (2, 4, 5, 7) are rare and treated as their unmirrored rotation.
pub(crate) fn orientation(app1: &[u8]) -> Option<Orientation> {
    let tiff = app1.strip_prefix(b"Exif\0\0")?;
    let big = match tiff.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let b: [u8; 2] = tiff.get(o..o + 2)?.try_into().ok()?;
        Some(if big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let b: [u8; 4] = tiff.get(o..o + 4)?.try_into().ok()?;
        Some(if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let entries = u16_at(ifd)? as usize;
    for i in 0..entries {
        let entry = ifd + 2 + i * 12;
        if u16_at(entry)? == 0x0112 {
            return Some(match u16_at(entry + 8)? {
                3 | 4 => Orientation::Rotate180,
                5 | 6 => Orientation::Rotate90,
                7 | 8 => Orientation::Rotate270,
                _ => Orientation::Normal,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn exif(big: bool, value: u16) -> Vec<u8> {
        let mut t = Vec::from(&b"Exif\0\0"[..]);
        let w16 = |t: &mut Vec<u8>, v: u16| t.extend(if big { v.to_be_bytes() } else { v.to_le_bytes() });
        let w32 = |t: &mut Vec<u8>, v: u32| t.extend(if big { v.to_be_bytes() } else { v.to_le_bytes() });
        t.extend(if big { b"MM" } else { b"II" });
        w16(&mut t, 42);
        w32(&mut t, 8);
        w16(&mut t, 2); // two entries
        for (tag, v) in [(0x010F, 0), (0x0112, value)] {
            w16(&mut t, tag);
            w16(&mut t, 3); // SHORT
            w32(&mut t, 1);
            w16(&mut t, v);
            w16(&mut t, 0);
        }
        t
    }

    #[test]
    fn reads_both_byte_orders() {
        assert_eq!(orientation(&exif(true, 6)), Some(Orientation::Rotate90));
        assert_eq!(orientation(&exif(false, 8)), Some(Orientation::Rotate270));
        assert_eq!(orientation(&exif(false, 3)), Some(Orientation::Rotate180));
        assert_eq!(orientation(&exif(true, 1)), Some(Orientation::Normal));
        assert_eq!(orientation(b"Exif\0\0junk"), None);
        assert_eq!(orientation(b"http://ns.adobe.com/xap/1.0/"), None);
    }
}
