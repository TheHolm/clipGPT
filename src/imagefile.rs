//! Clipboard images: work out the real format, convert to something every
//! OpenAI-compatible server decodes, and encode as a `data:` URL.
//!
//! PNG and JPEG are not re-encoded (re-encoding a photo as PNG would only
//! make it bigger), but their metadata is removed at the byte level by
//! [`strip_png`] and [`strip_jpeg`]: EXIF (camera, time, GPS position),
//! XMP, text chunks, comments, thumbnails and anything after the end of the
//! image. Only what is needed to show the picture correctly is kept. WebP,
//! GIF (first frame), BMP, Windows DIB and TIFF are decoded and re-encoded
//! as PNG, which carries no metadata: llama.cpp's image loader, for one,
//! cannot read WebP, and OpenAI accepts neither BMP nor TIFF.
//!
//! The format is taken from the data's first bytes, never from the name
//! the clipboard owner gave it. Decoding is bounded by [`MAX_DIMENSION`]
//! and [`MAX_DECODE_ALLOC`], so a small file declaring a gigantic picture
//! cannot exhaust memory. Error messages never contain image data.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use image::codecs::bmp::BmpDecoder;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use std::io::Cursor;

/// Largest width or height, in pixels, of an image that is converted.
const MAX_DIMENSION: u32 = 8192;

/// Most memory the decoder may allocate while converting one image.
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// An image as read from the clipboard, not yet checked.
#[derive(Debug, PartialEq, Eq)]
pub enum ClipImage {
    /// The bytes of an image file (format still to be sniffed).
    File(Vec<u8>),
    /// A Windows device-independent bitmap (`CF_DIB`): a BMP file without
    /// its 14-byte file header.
    #[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
    Dib(Vec<u8>),
}

/// An image ready to send: PNG or JPEG bytes and their MIME type.
#[derive(Debug, PartialEq, Eq)]
pub struct Prepared {
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

impl Prepared {
    /// The image as a base64 `data:` URL, as used in an `image_url` part.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime, STANDARD.encode(&self.bytes))
    }
}

/// Image file formats recognised by their first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Png,
    Jpeg,
    WebP,
    Gif,
    Bmp,
    Tiff,
}

/// Recognise an image file format from its signature.
fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(Format::Gif)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(Format::WebP)
    } else if bytes.starts_with(b"BM") {
        Some(Format::Bmp)
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        Some(Format::Tiff)
    } else {
        None
    }
}

/// Error for an image over `max_image_bytes`. Only sizes are reported.
pub fn too_large(max_bytes: usize) -> String {
    format!(
        "Clipboard image is larger than max_image_bytes ({} bytes); not sent",
        max_bytes
    )
}

/// The decoder limits used for every conversion.
fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    limits
}

/// Error for a decode failure of `what` (a format name). The decoder's
/// message only describes the problem, never pixel data.
fn decode_error(what: &str, err: image::ImageError) -> String {
    match err {
        image::ImageError::Limits(_) => format!(
            "Clipboard image ({what}) is too big to convert (over {MAX_DIMENSION}x{MAX_DIMENSION} \
             pixels or {} MiB to decode); not sent",
            MAX_DECODE_ALLOC / (1024 * 1024)
        ),
        other => format!("Could not decode clipboard image ({what}): {other}; not sent"),
    }
}

/// Re-encode a decoded image as 8-bit PNG (with alpha only if it has any
/// channel for it), rejecting a result over `max_bytes`.
fn encode_png(img: DynamicImage, max_bytes: usize) -> Result<Prepared, String> {
    let img = if img.color().has_alpha() {
        DynamicImage::ImageRgba8(img.to_rgba8())
    } else {
        DynamicImage::ImageRgb8(img.to_rgb8())
    };
    let mut bytes = Vec::new();
    img.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .map_err(|e| format!("Could not convert clipboard image to PNG: {e}"))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "Clipboard image converted to PNG is larger than max_image_bytes ({} bytes); not sent",
            max_bytes
        ));
    }
    Ok(Prepared {
        mime: "image/png",
        bytes,
    })
}

/// Decode a file of a convertible `format` and re-encode it as PNG.
fn convert_file(bytes: &[u8], format: ImageFormat, max_bytes: usize) -> Result<Prepared, String> {
    let what = format.to_mime_type();
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits());
    let img = reader.decode().map_err(|e| decode_error(what, e))?;
    encode_png(img, max_bytes)
}

/// Decode a Windows DIB and re-encode it as PNG.
fn convert_dib(bytes: &[u8], max_bytes: usize) -> Result<Prepared, String> {
    let what = "Windows bitmap";
    let mut decoder = BmpDecoder::new_without_file_header(Cursor::new(bytes))
        .map_err(|e| decode_error(what, e))?;
    decoder
        .set_limits(limits())
        .map_err(|e| decode_error(what, e))?;
    let img = DynamicImage::from_decoder(decoder).map_err(|e| decode_error(what, e))?;
    encode_png(img, max_bytes)
}

/// Error for a PNG or JPEG whose structure could not be walked.
fn malformed(what: &str) -> String {
    format!("Clipboard image ({what}) is malformed; not sent")
}

/// PNG ancillary chunks that are kept: those that affect how the pixels
/// look (transparency, colour space, gamma, significant bits, pixel
/// aspect). Every other ancillary chunk - text (tEXt, zTXt, iTXt), EXIF
/// (eXIf), time (tIME), animation frames (acTL, fcTL, fdAT: the default
/// image stays) and unknown private chunks - is dropped.
const PNG_KEPT_ANCILLARY: [&[u8; 4]; 9] = [
    b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"sBIT", b"pHYs", b"cICP", b"bKGD",
];

/// Return `png` (starting with the PNG signature) with every chunk removed
/// except the critical ones (IHDR, PLTE, IDAT, IEND) and
/// [`PNG_KEPT_ANCILLARY`], and nothing after IEND. Chunk CRCs are copied,
/// not checked; decoding is left to the server. A truncated file, or one
/// without IHDR first or without IEND, is an error.
fn strip_png(png: &[u8]) -> Result<Vec<u8>, String> {
    const SIGNATURE: usize = 8;
    let bad = || malformed("PNG");
    let mut out = png.get(..SIGNATURE).ok_or_else(bad)?.to_vec();
    let mut pos = SIGNATURE;
    let mut first = true;
    loop {
        let header = png.get(pos..pos + 8).ok_or_else(bad)?;
        let len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let kind: &[u8; 4] = header[4..8].try_into().unwrap();
        let end = pos
            .checked_add(12)
            .and_then(|p| p.checked_add(len))
            .filter(|&e| e <= png.len())
            .ok_or_else(bad)?;
        if first && kind != b"IHDR" {
            return Err(bad());
        }
        first = false;
        // Bit 5 of the first type byte clear (upper case) = critical.
        let critical = kind[0].is_ascii_uppercase();
        if critical || PNG_KEPT_ANCILLARY.contains(&kind) {
            out.extend_from_slice(&png[pos..end]);
        }
        if kind == b"IEND" {
            return Ok(out);
        }
        pos = end;
    }
}

/// Whether the JPEG segment with `marker` (the byte after 0xFF) and
/// `payload` (the bytes after its length field) is kept. APP0 (JFIF),
/// APP2 holding an ICC profile and APP14 (Adobe, needed to decode CMYK
/// colours right) are kept; every other APPn (EXIF and XMP in APP1, MPF
/// in APP2, ...) and COM (comments) are dropped. All other segments
/// (tables, frame and scan headers) are image data and kept.
fn jpeg_segment_kept(marker: u8, payload: &[u8]) -> bool {
    match marker {
        0xE0 => true,
        0xE2 => payload.starts_with(b"ICC_PROFILE\0"),
        0xEE => payload.starts_with(b"Adobe"),
        0xE1..=0xEF | 0xFE => false,
        _ => true,
    }
}

/// Return `jpeg` (starting with SOI) with the metadata segments that
/// [`jpeg_segment_kept`] rejects removed - before, between and after
/// scans - and nothing after EOI (where e.g. MPF stores extra images with
/// their own EXIF). Entropy-coded scan data is copied unchanged. Fill
/// bytes before a marker are dropped. A truncated file, or one without
/// EOI, is an error.
fn strip_jpeg(jpeg: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || malformed("JPEG");
    if !jpeg.starts_with(&[0xFF, 0xD8]) {
        return Err(bad());
    }
    let mut out = vec![0xFF, 0xD8];
    let mut pos = 2;
    loop {
        // A marker: 0xFF (any number of them as fill), then its code.
        if jpeg.get(pos) != Some(&0xFF) {
            return Err(bad());
        }
        while jpeg.get(pos) == Some(&0xFF) {
            pos += 1;
        }
        let marker = *jpeg.get(pos).ok_or_else(bad)?;
        pos += 1;
        match marker {
            0xD9 => {
                out.extend_from_slice(&[0xFF, 0xD9]);
                return Ok(out);
            }
            // Markers without a length: restart markers and TEM.
            0xD0..=0xD7 | 0x01 => out.extend_from_slice(&[0xFF, marker]),
            // A second SOI, or a stray 0xFF00 outside scan data.
            0xD8 | 0x00 => return Err(bad()),
            _ => {
                let len_bytes = jpeg.get(pos..pos + 2).ok_or_else(bad)?;
                let len = u16::from_be_bytes([len_bytes[0], len_bytes[1]]) as usize;
                if len < 2 {
                    return Err(bad());
                }
                let end = pos + len;
                let segment = jpeg.get(pos..end).ok_or_else(bad)?;
                if jpeg_segment_kept(marker, &segment[2..]) {
                    out.extend_from_slice(&[0xFF, marker]);
                    out.extend_from_slice(segment);
                }
                pos = end;
                if marker == 0xDA {
                    // Start of scan: entropy-coded data up to the next
                    // marker that is neither a stuffed 0xFF00 nor a
                    // restart marker.
                    let start = pos;
                    loop {
                        match (jpeg.get(pos), jpeg.get(pos + 1)) {
                            (Some(0xFF), Some(0x00 | 0xD0..=0xD7)) => pos += 2,
                            (Some(0xFF), Some(_)) => break,
                            (Some(_), _) => pos += 1,
                            (None, _) => return Err(bad()),
                        }
                    }
                    out.extend_from_slice(&jpeg[start..pos]);
                }
            }
        }
    }
}

/// Check a clipboard image and turn it into PNG or JPEG of at most
/// `max_bytes`, without metadata. Unknown formats are refused.
pub fn prepare(image: ClipImage, max_bytes: usize) -> Result<Prepared, String> {
    let bytes = match image {
        ClipImage::Dib(bytes) => {
            if bytes.len() > max_bytes {
                return Err(too_large(max_bytes));
            }
            return convert_dib(&bytes, max_bytes);
        }
        ClipImage::File(bytes) => bytes,
    };
    if bytes.len() > max_bytes {
        return Err(too_large(max_bytes));
    }
    match sniff(&bytes) {
        Some(Format::Png) => Ok(Prepared {
            mime: "image/png",
            bytes: strip_png(&bytes)?,
        }),
        Some(Format::Jpeg) => Ok(Prepared {
            mime: "image/jpeg",
            bytes: strip_jpeg(&bytes)?,
        }),
        Some(Format::WebP) => convert_file(&bytes, ImageFormat::WebP, max_bytes),
        Some(Format::Gif) => convert_file(&bytes, ImageFormat::Gif, max_bytes),
        Some(Format::Bmp) => convert_file(&bytes, ImageFormat::Bmp, max_bytes),
        Some(Format::Tiff) => convert_file(&bytes, ImageFormat::Tiff, max_bytes),
        None => Err(
            "Clipboard image format is not supported (PNG, JPEG, WebP, GIF, BMP and \
                     TIFF are); not sent"
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    /// A 3x2 test picture with distinct pixels.
    fn picture() -> RgbImage {
        RgbImage::from_fn(3, 2, |x, y| Rgb([x as u8 * 80, y as u8 * 120, 7]))
    }

    /// `picture()` encoded as `format`.
    fn encoded(format: ImageFormat) -> Vec<u8> {
        let img = match format {
            // GIF and WebP (lossless) round-trip RGBA exactly.
            ImageFormat::Gif => DynamicImage::ImageRgba8(DynamicImage::from(picture()).to_rgba8()),
            _ => DynamicImage::from(picture()),
        };
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), format).unwrap();
        out
    }

    /// Decode PNG output and compare its RGB pixels with `picture()`.
    fn assert_same_picture(png: &[u8]) {
        let back = image::load_from_memory_with_format(png, ImageFormat::Png)
            .unwrap()
            .to_rgb8();
        assert_eq!(back, picture());
    }

    #[test]
    fn sniffs_signatures() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), Some(Format::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
        assert_eq!(sniff(b"GIF89a.."), Some(Format::Gif));
        assert_eq!(sniff(b"GIF87a.."), Some(Format::Gif));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8L"), Some(Format::WebP));
        assert_eq!(sniff(b"RIFF\0\0\0\0WAVE"), None);
        assert_eq!(sniff(b"BM...."), Some(Format::Bmp));
        assert_eq!(sniff(b"II*\0...."), Some(Format::Tiff));
        assert_eq!(sniff(b"MM\0*...."), Some(Format::Tiff));
        assert_eq!(sniff(b"<svg"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn png_and_jpeg_without_metadata_pass_unchanged() {
        let png = encoded(ImageFormat::Png);
        let got = prepare(ClipImage::File(png.clone()), 1 << 20).unwrap();
        assert_eq!((got.mime, got.bytes), ("image/png", png));
        let jpeg = jpeg_from(&[&jfif(), &dqt_sof_dht(), &scan()]);
        let got = prepare(ClipImage::File(jpeg.clone()), 1 << 20).unwrap();
        assert_eq!((got.mime, got.bytes), ("image/jpeg", jpeg));
    }

    /// A PNG chunk: length, type, data and a (dummy) CRC.
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut c = (data.len() as u32).to_be_bytes().to_vec();
        c.extend_from_slice(kind);
        c.extend_from_slice(data);
        c.extend_from_slice(&[0xAB; 4]);
        c
    }

    /// `png` with `chunks` inserted after IHDR (which ends at byte 33).
    fn png_with(png: &[u8], chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = png[..33].to_vec();
        for c in chunks {
            out.extend_from_slice(c);
        }
        out.extend_from_slice(&png[33..]);
        out
    }

    #[test]
    fn png_metadata_is_removed() {
        let png = encoded(ImageFormat::Png);
        assert_eq!(&png[12..16], b"IHDR");
        let mut tagged = png_with(
            &png,
            &[
                chunk(b"tEXt", b"Author\0SECRET-NAME"),
                chunk(b"iTXt", b"XML:com.adobe.xmp\0\0\0\0\0SECRET-XMP"),
                chunk(b"eXIf", b"MM\0*SECRET-GPS"),
                chunk(b"tIME", &[7, 0xEA, 1, 1, 0, 0, 0]),
                chunk(b"prVt", b"SECRET-PRIVATE"),
                chunk(b"gAMA", &[0, 0, 0xB1, 0x8F]),
            ],
        );
        tagged.extend_from_slice(b"SECRET-TRAILER");
        let got = prepare(ClipImage::File(tagged), 1 << 20).unwrap();
        assert!(!got.bytes.windows(6).any(|w| w == b"SECRET"));
        // The colour chunk stays; the result is the original plus it.
        assert_eq!(
            got.bytes,
            png_with(&png, &[chunk(b"gAMA", &[0, 0, 0xB1, 0x8F])])
        );
        assert_same_picture(&got.bytes);
    }

    #[test]
    fn broken_png_is_refused() {
        let png = encoded(ImageFormat::Png);
        // Truncated (no IEND), a chunk running past the end, no IHDR first.
        for bad in [
            png[..png.len() - 12].to_vec(),
            png[..40].to_vec(),
            [&png[..8], &chunk(b"IEND", b"")[..]].concat(),
            b"\x89PNG\r\n\x1a\n".to_vec(),
        ] {
            let err = prepare(ClipImage::File(bad), 1 << 20).unwrap_err();
            assert!(err.contains("(PNG) is malformed"), "{err}");
        }
        let mut huge = png[..8].to_vec();
        huge.extend_from_slice(&chunk(b"IHDR", &[0; 13]));
        huge[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(prepare(ClipImage::File(huge), 1 << 20).is_err());
    }

    /// A JPEG segment: 0xFF, `marker`, length and `payload`.
    fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut s = vec![0xFF, marker];
        s.extend_from_slice(&(payload.len() as u16 + 2).to_be_bytes());
        s.extend_from_slice(payload);
        s
    }

    /// The JFIF APP0 segment.
    fn jfif() -> Vec<u8> {
        segment(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0")
    }

    /// Fake quantisation table, frame header and Huffman table segments
    /// (structure only; the image is never decoded here).
    fn dqt_sof_dht() -> Vec<u8> {
        [
            segment(0xDB, &[0; 65]),
            segment(0xC0, &[8, 0, 2, 0, 3, 1, 1, 0x11, 0]),
            segment(0xC4, &[0; 17]),
        ]
        .concat()
    }

    /// A scan header plus entropy-coded data containing a stuffed 0xFF00
    /// and a restart marker, which are data, not segment boundaries.
    fn scan() -> Vec<u8> {
        let mut s = segment(0xDA, &[1, 1, 0, 0, 0x3F, 0]);
        s.extend_from_slice(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56, 0xFE]);
        s
    }

    /// SOI, `parts`, EOI.
    fn jpeg_from(parts: &[&[u8]]) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        for p in parts {
            j.extend_from_slice(p);
        }
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    #[test]
    fn jpeg_metadata_is_removed() {
        let icc = segment(0xE2, b"ICC_PROFILE\0\x01\x01profile");
        let adobe = segment(0xEE, b"Adobe\0\x64\0\0\0\0\x01");
        let tagged = jpeg_from(&[
            &jfif(),
            &segment(0xE1, b"Exif\0\0MM\0*SECRET-GPS"),
            &segment(0xE1, b"http://ns.adobe.com/xap/1.0/\0SECRET-XMP"),
            &segment(0xE2, b"MPF\0SECRET-MPF"),
            &icc,
            &adobe,
            &segment(0xED, b"Photoshop 3.0\0SECRET-IPTC"),
            &segment(0xFE, b"SECRET-COMMENT"),
            &[0xFF, 0xFF], // fill bytes before the next marker
            &dqt_sof_dht(),
            &scan(),
            // A comment between progressive scans is dropped too.
            &segment(0xFE, b"SECRET-BETWEEN-SCANS"),
            &scan(),
        ]);
        let mut with_trailer = tagged.clone();
        with_trailer.extend_from_slice(b"\xFF\xD8SECRET-SECOND-IMAGE\xFF\xD9");
        let got = prepare(ClipImage::File(with_trailer), 1 << 20).unwrap();
        assert_eq!(got.mime, "image/jpeg");
        assert!(!got.bytes.windows(6).any(|w| w == b"SECRET"));
        assert_eq!(
            got.bytes,
            jpeg_from(&[&jfif(), &icc, &adobe, &dqt_sof_dht(), &scan(), &scan()])
        );
    }

    #[test]
    fn broken_jpeg_is_refused() {
        let good = jpeg_from(&[&jfif(), &dqt_sof_dht(), &scan()]);
        let err = prepare(ClipImage::File(good[..10].to_vec()), 1 << 20).unwrap_err();
        assert!(err.contains("(JPEG) is malformed"), "{err}");
        let mut bad_len = good.clone();
        bad_len[5] = 1; // APP0 length 1
        for bad in [
            vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3],
            vec![0xFF, 0xD8, 0xFF],
            good[..good.len() - 2].to_vec(), // no EOI
            good[..10].to_vec(),             // segment cut off
            bad_len,
            jpeg_from(&[&[0x00, 0x11]]), // data where a marker belongs
            jpeg_from(&[&[0xFF, 0xD8]]), // second SOI
        ] {
            let err = strip_jpeg(&bad).unwrap_err();
            assert!(err.contains("(JPEG) is malformed"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn other_formats_become_png() {
        for format in [
            ImageFormat::WebP,
            ImageFormat::Gif,
            ImageFormat::Bmp,
            ImageFormat::Tiff,
        ] {
            let got = prepare(ClipImage::File(encoded(format)), 1 << 20)
                .unwrap_or_else(|e| panic!("{format:?}: {e}"));
            assert_eq!(got.mime, "image/png", "{format:?}");
            assert_eq!(sniff(&got.bytes), Some(Format::Png), "{format:?}");
            assert_same_picture(&got.bytes);
        }
    }

    #[test]
    fn alpha_is_kept_only_when_present() {
        let rgba = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 100]));
        let mut gif = Vec::new();
        DynamicImage::ImageRgba8(rgba)
            .write_to(&mut Cursor::new(&mut gif), ImageFormat::Gif)
            .unwrap();
        let png = prepare(ClipImage::File(gif), 1 << 20).unwrap().bytes;
        let back = image::load_from_memory(&png).unwrap();
        assert!(back.color().has_alpha());
        let png = prepare(ClipImage::File(encoded(ImageFormat::Bmp)), 1 << 20)
            .unwrap()
            .bytes;
        assert!(!image::load_from_memory(&png).unwrap().color().has_alpha());
    }

    #[test]
    fn windows_dib_becomes_png() {
        let bmp = encoded(ImageFormat::Bmp);
        let dib = bmp[14..].to_vec();
        let got = prepare(ClipImage::Dib(dib), 1 << 20).unwrap();
        assert_eq!(got.mime, "image/png");
        assert_same_picture(&got.bytes);
    }

    #[test]
    fn unknown_or_broken_images_are_refused() {
        let err = prepare(ClipImage::File(b"<svg></svg>".to_vec()), 100).unwrap_err();
        assert!(err.contains("not supported"), "{err}");
        let err = prepare(ClipImage::File(b"GIF89a garbage".to_vec()), 100).unwrap_err();
        assert!(err.contains("Could not decode"), "{err}");
        let err = prepare(ClipImage::Dib(vec![1, 2, 3]), 100).unwrap_err();
        assert!(err.contains("Windows bitmap"), "{err}");
    }

    #[test]
    fn raw_size_limit() {
        let png = encoded(ImageFormat::Png);
        let n = png.len();
        assert!(prepare(ClipImage::File(png.clone()), n).is_ok());
        assert_eq!(prepare(ClipImage::File(png), n - 1), Err(too_large(n - 1)));
        assert_eq!(prepare(ClipImage::Dib(vec![0; 10]), 9), Err(too_large(9)));
    }

    #[test]
    fn converted_size_limit() {
        let img = DynamicImage::from(picture());
        let err = encode_png(img.clone(), 10).unwrap_err();
        assert!(
            err.contains("converted to PNG") && err.contains("max_image_bytes (10 bytes)"),
            "{err}"
        );
        let png = encode_png(img, 1 << 20).unwrap().bytes;
        assert!(encode_png(DynamicImage::from(picture()), png.len()).is_ok());
    }

    /// A BMP/DIB header declaring a `width` x `height` 24-bit picture but
    /// carrying no pixel data.
    fn huge_bmp(width: i32, height: i32) -> Vec<u8> {
        let mut info = Vec::new();
        info.extend_from_slice(&40u32.to_le_bytes()); // header size
        info.extend_from_slice(&width.to_le_bytes());
        info.extend_from_slice(&height.to_le_bytes());
        info.extend_from_slice(&1u16.to_le_bytes()); // planes
        info.extend_from_slice(&24u16.to_le_bytes()); // bits per pixel
        info.extend_from_slice(&[0; 24]); // BI_RGB, sizes, resolution, palette
        let mut file = b"BM".to_vec();
        file.extend_from_slice(&(54u32 + 16).to_le_bytes());
        file.extend_from_slice(&[0; 4]);
        file.extend_from_slice(&54u32.to_le_bytes());
        file.extend_from_slice(&info);
        file.extend_from_slice(&[0; 16]);
        file
    }

    #[test]
    fn huge_declared_dimensions_are_refused_without_allocating() {
        let file = huge_bmp(60_000, 60_000);
        let err = prepare(ClipImage::File(file.clone()), 1 << 20).unwrap_err();
        assert!(err.contains("too big to convert"), "{err}");
        let err = prepare(ClipImage::Dib(file[14..].to_vec()), 1 << 20).unwrap_err();
        assert!(err.contains("too big to convert"), "{err}");
    }

    #[test]
    fn data_url_format() {
        let p = Prepared {
            mime: "image/png",
            bytes: b"hi!".to_vec(),
        };
        assert_eq!(p.data_url(), "data:image/png;base64,aGkh");
    }
}
