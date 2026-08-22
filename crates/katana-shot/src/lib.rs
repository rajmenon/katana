//! Screenshot geometry and encode helpers. Capture adapters stay Win32-side.

pub use katana_core::{parse_shot_rest, ShotMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub dpi: u32,
}

impl PhysRect {
    pub fn is_empty(self) -> bool {
        self.w <= 0 || self.h <= 0
    }
}

/// Convert logical (96-DPI) rect to physical pixels. Never apply twice.
pub fn logical_to_physical(x: i32, y: i32, w: i32, h: i32, dpi: u32) -> PhysRect {
    let dpi = if dpi == 0 { 96 } else { dpi };
    if dpi == 96 {
        return PhysRect { x, y, w, h, dpi };
    }
    let scale = dpi as i64;
    let conv = |v: i32| -> i32 { ((v as i64) * scale / 96) as i32 };
    PhysRect {
        x: conv(x),
        y: conv(y),
        w: conv(w),
        h: conv(h),
        dpi,
    }
}

/// Identity when the rect is already tagged with its physical dpi.
pub fn ensure_physical(r: PhysRect) -> PhysRect {
    r
}

pub fn intersect(a: PhysRect, b: PhysRect) -> Option<PhysRect> {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.w).min(b.x + b.w);
    let y2 = (a.y + a.h).min(b.y + b.h);
    let w = x2 - x1;
    let h = y2 - y1;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(PhysRect {
        x: x1,
        y: y1,
        w,
        h,
        dpi: a.dpi,
    })
}

/// Device-independent BITMAPINFOHEADER + BGRA pixels (bottom-up DIB).
pub fn dib_from_bgra(width: u32, height: u32, bgra: &[u8]) -> Result<Vec<u8>, String> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "overflow".to_string())?;
    if bgra.len() != expected {
        return Err(format!(
            "bgra len {} != {}x{}x4",
            bgra.len(),
            width,
            height
        ));
    }
    let row_stride = (width as usize) * 4;
    let mut out = Vec::with_capacity(40 + expected);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes()); // positive = bottom-up
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bitcount
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(expected as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    // flip to bottom-up
    for y in (0..height as usize).rev() {
        let s = y * row_stride;
        out.extend_from_slice(&bgra[s..s + row_stride]);
    }
    Ok(out)
}

/// Minimal PNG (no filter, RGB) so capture can write a file without the `image` crate.
pub fn png_from_bgra(width: u32, height: u32, bgra: &[u8]) -> Result<Vec<u8>, String> {
    let expected = width as usize * height as usize * 4;
    if bgra.len() != expected {
        return Err("size mismatch".into());
    }
    let mut raw = Vec::with_capacity((width as usize * 3 + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0); // filter None
        for x in 0..width as usize {
            let i = (y * width as usize + x) * 4;
            raw.push(bgra[i + 2]); // R from BGRA
            raw.push(bgra[i + 1]);
            raw.push(bgra[i]);
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
    write_chunk(&mut out, *b"IHDR", &{
        let mut d = Vec::new();
        d.extend_from_slice(&width.to_be_bytes());
        d.extend_from_slice(&height.to_be_bytes());
        d.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
        d
    });
    let deflated = deflate_store(&raw);
    write_chunk(&mut out, *b"IDAT", &deflated);
    write_chunk(&mut out, *b"IEND", &[]);
    Ok(out)
}

fn write_chunk(out: &mut Vec<u8>, ty: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(&ty);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(&ty);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// zlib wrapper around uncompressed DEFLATE stored blocks (correct, tiny).
fn deflate_store(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // zlib, stored
    let mut i = 0;
    while i < data.len() {
        let n = (data.len() - i).min(65535);
        let last = i + n == data.len();
        out.push(if last { 1 } else { 0 });
        let n16 = n as u16;
        out.extend_from_slice(&n16.to_le_bytes());
        out.extend_from_slice(&(!n16).to_le_bytes());
        out.extend_from_slice(&data[i..i + n]);
        i += n;
    }
    let adler = adler32(data);
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

/// Decode a PNG produced by [`png_from_bgra`] (8-bit RGB, filter None, stored DEFLATE).
pub fn png_to_bgra(png: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    if png.len() < 33 || png[..8] != [137, 80, 78, 71, 13, 10, 26, 10] {
        return Err("not png".into());
    }
    let mut i = 8usize;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut color = 0u8;
    let mut idat = Vec::new();
    while i + 8 <= png.len() {
        let len = u32::from_be_bytes(png[i..i + 4].try_into().unwrap()) as usize;
        if i + 12 + len > png.len() {
            return Err("truncated png".into());
        }
        let ty = &png[i + 4..i + 8];
        let data = &png[i + 8..i + 8 + len];
        if ty == b"IHDR" && data.len() >= 13 {
            width = u32::from_be_bytes(data[0..4].try_into().unwrap());
            height = u32::from_be_bytes(data[4..8].try_into().unwrap());
            color = data[9];
            if data[8] != 8 {
                return Err("only 8-bit png".into());
            }
        } else if ty == b"IDAT" {
            idat.extend_from_slice(data);
        } else if ty == b"IEND" {
            break;
        }
        i += 12 + len;
    }
    if width == 0 || height == 0 {
        return Err("bad ihdr".into());
    }
    let raw = inflate_zlib_stored(&idat)?;
    let bpp = match color {
        2 => 3,
        6 => 4,
        _ => return Err("unsupported png color".into()),
    };
    let row = 1 + width as usize * bpp;
    if raw.len() != row * height as usize {
        return Err("png size mismatch".into());
    }
    let mut bgra = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        if raw[y * row] != 0 {
            return Err("filtered png".into());
        }
        for x in 0..width as usize {
            let s = y * row + 1 + x * bpp;
            let d = (y * width as usize + x) * 4;
            bgra[d] = raw[s + 2];
            bgra[d + 1] = raw[s + 1];
            bgra[d + 2] = raw[s];
            bgra[d + 3] = if bpp == 4 { raw[s + 3] } else { 255 };
        }
    }
    Ok((width, height, bgra))
}

fn inflate_zlib_stored(z: &[u8]) -> Result<Vec<u8>, String> {
    if z.len() < 6 {
        return Err("short zlib".into());
    }
    let mut i = 2usize;
    let mut out = Vec::new();
    loop {
        if i + 5 > z.len() {
            return Err("short stored block".into());
        }
        let header = z[i];
        i += 1;
        let n = u16::from_le_bytes([z[i], z[i + 1]]) as usize;
        i += 4;
        if i + n > z.len() {
            return Err("stored overflow".into());
        }
        out.extend_from_slice(&z[i..i + n]);
        i += n;
        if header & 1 != 0 {
            break;
        }
    }
    Ok(out)
}

/// Nearest-neighbor scale for clipboard thumbnails.
pub fn scale_bgra(w: u32, h: u32, src: &[u8], nw: u32, nh: u32) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 || nw == 0 || nh == 0 {
        return Err("empty".into());
    }
    if src.len() != w as usize * h as usize * 4 {
        return Err("size".into());
    }
    let mut out = vec![0u8; nw as usize * nh as usize * 4];
    for y in 0..nh as usize {
        let sy = y * h as usize / nh as usize;
        for x in 0..nw as usize {
            let sx = x * w as usize / nw as usize;
            let s = (sy * w as usize + sx) * 4;
            let d = (y * nw as usize + x) * 4;
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    Ok(out)
}

/// CF_DIB / our DIB (BITMAPINFOHEADER + 32-bpp pixels) → top-down BGRA.
pub fn bgra_from_dib(dib: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    if dib.len() < 40 {
        return Err("short dib".into());
    }
    let header = u32::from_le_bytes(dib[0..4].try_into().unwrap()) as usize;
    if header < 40 || dib.len() < header {
        return Err("bad dib header".into());
    }
    let width = i32::from_le_bytes(dib[4..8].try_into().unwrap()).unsigned_abs();
    let height_i = i32::from_le_bytes(dib[8..12].try_into().unwrap());
    let top_down = height_i < 0;
    let height = height_i.unsigned_abs();
    let bitcount = u16::from_le_bytes(dib[14..16].try_into().unwrap());
    if bitcount != 32 && bitcount != 24 {
        return Err("dib bitcount".into());
    }
    let bpp = bitcount as usize / 8;
    let stride = ((width as usize * bpp + 3) / 4) * 4;
    let pixels = &dib[header..];
    if pixels.len() < stride * height as usize {
        return Err("short dib pixels".into());
    }
    let mut bgra = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        let src_y = if top_down {
            y
        } else {
            height as usize - 1 - y
        };
        let row = &pixels[src_y * stride..];
        for x in 0..width as usize {
            let s = x * bpp;
            let d = (y * width as usize + x) * 4;
            bgra[d] = row[s];
            bgra[d + 1] = row[s + 1];
            bgra[d + 2] = row[s + 2];
            bgra[d + 3] = if bpp == 4 { row[s + 3] } else { 255 };
        }
    }
    Ok((width, height, bgra))
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

struct Crc32 {
    v: u32,
}

impl Crc32 {
    fn new() -> Self {
        Self { v: 0xffffffff }
    }
    fn update(&mut self, data: &[u8]) {
        let mut c = self.v;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb88320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        self.v = c;
    }
    fn finish(self) -> u32 {
        self.v ^ 0xffffffff
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_does_not_mix_spaces() {
        let logical = logical_to_physical(10, 20, 100, 50, 96);
        assert_eq!(logical, PhysRect { x: 10, y: 20, w: 100, h: 50, dpi: 96 });
        let hi = logical_to_physical(10, 20, 100, 50, 192);
        assert_eq!(hi.x, 20);
        assert_eq!(hi.y, 40);
        assert_eq!(hi.w, 200);
        assert_eq!(hi.h, 100);
        assert_eq!(hi.dpi, 192);
        // applying ensure_physical does not rescale again
        assert_eq!(ensure_physical(hi), hi);
    }

    #[test]
    fn dib_header_matches_pixels() {
        let w = 2u32;
        let h = 2u32;
        // BGRA
        let px = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 1, 2, 3, 255,
        ];
        let dib = dib_from_bgra(w, h, &px).unwrap();
        assert_eq!(&dib[0..4], &40u32.to_le_bytes());
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 32);
        assert_eq!(dib.len(), 40 + 16);
    }

    #[test]
    fn png_roundtrip_bgra() {
        let w = 3u32;
        let h = 2u32;
        let mut px = vec![0u8; 24];
        px[0] = 10;
        px[1] = 20;
        px[2] = 30;
        px[3] = 255;
        px[20] = 7;
        px[21] = 8;
        px[22] = 9;
        px[23] = 255;
        let png = png_from_bgra(w, h, &px).unwrap();
        let (rw, rh, back) = png_to_bgra(&png).unwrap();
        assert_eq!((rw, rh), (w, h));
        assert_eq!(&back[0..4], &px[0..4]);
        assert_eq!(&back[20..24], &px[20..24]);
    }

    #[test]
    fn scale_and_dib_roundtrip() {
        let px = vec![10u8, 20, 30, 255, 1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255];
        let small = scale_bgra(2, 2, &px, 1, 1).unwrap();
        assert_eq!(small, [10, 20, 30, 255]);
        let dib = dib_from_bgra(2, 2, &px).unwrap();
        let (w, h, back) = bgra_from_dib(&dib).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(back, px);
    }

    #[test]
    fn png_has_signature_and_ihdr() {
        let px = vec![0u8; 4];
        let png = png_from_bgra(1, 1, &px).unwrap();
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        assert_eq!(&png[12..16], b"IHDR");
        let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert_eq!((w, h), (1, 1));
    }

    #[test]
    fn shot_modes_from_core() {
        assert_eq!(parse_shot_rest("last"), ShotMode::Last);
        assert_eq!(parse_shot_rest("window"), ShotMode::Window);
        assert_eq!(parse_shot_rest("screen"), ShotMode::Screen);
        assert_eq!(parse_shot_rest(""), ShotMode::Region);
    }
}
