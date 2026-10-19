//! Clipboard images: work out the real format, convert to something every
//! OpenAI-compatible server decodes, and encode as a `data:` URL.
//!
//! PNG and JPEG are sent unchanged (re-encoding a photo as PNG would only
//! make it bigger). WebP, GIF (first frame), BMP, Windows DIB and TIFF are
//! decoded and re-encoded as PNG: llama.cpp's image loader, for one, cannot
//! read WebP, and OpenAI accepts neither BMP nor TIFF.
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

/// Check a clipboard image and turn it into PNG or JPEG of at most
/// `max_bytes`. Unknown formats are refused.
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
            bytes,
        }),
        Some(Format::Jpeg) => Ok(Prepared {
            mime: "image/jpeg",
            bytes,
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
    fn png_and_jpeg_pass_unchanged() {
        let png = encoded(ImageFormat::Png);
        let got = prepare(ClipImage::File(png.clone()), 1 << 20).unwrap();
        assert_eq!((got.mime, got.bytes), ("image/png", png));
        // Only the signature is looked at for JPEG; it is not decoded.
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];
        let got = prepare(ClipImage::File(jpeg.clone()), 100).unwrap();
        assert_eq!((got.mime, got.bytes), ("image/jpeg", jpeg));
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
