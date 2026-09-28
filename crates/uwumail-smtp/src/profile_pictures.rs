//! Profile pictures as files (docs/profile-pictures.md): what people upload is decoded within
//! limits, cut to the square in its middle, scaled down and written anew, so nothing the file carried
//! besides its pixels — camera, place, editing history — ever leaves the server. The small PNG of the
//! `Face:` header is made here too, and a Face that came with a message is checked before it is kept.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, RgbImage, RgbaImage};

/// The largest upload taken, as JMAP's `maxSize` says.
pub const MAX_UPLOAD_BYTES: usize = 10 * 1024 * 1024;
/// The largest width and height an upload may have.
pub const MAX_DIMENSION: u32 = 8000;
/// What a stored picture is scaled down to at most.
pub const STORED_SIZE: u32 = 512;
/// Width and height of the `Face:` PNG.
pub const FACE_SIZE: u32 = 48;
/// The most a `Face:` PNG may take, so its base64 fits the header (quimby.gnus.org/circus/face).
pub const MAX_FACE_BYTES: usize = 725;
/// A Face from somewhere else is kept up to this size: a little slack over the limit for encoders
/// that miss it, never more.
pub const MAX_INCOMING_FACE_BYTES: usize = 2048;
/// Memory the decoder may take: an 8000 × 8000 picture with an alpha channel, and a little more.
const MAX_DECODER_BYTES: u64 = MAX_DIMENSION as u64 * MAX_DIMENSION as u64 * 4 + 16 * 1024 * 1024;
const JPEG_QUALITY: u8 = 88;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PictureError {
    #[error("the file is empty")]
    Empty,
    #[error("the file is larger than {} MB", MAX_UPLOAD_BYTES / 1024 / 1024)]
    TooLarge,
    #[error("PNG, JPEG, WebP or GIF")]
    NotAPicture,
    #[error("at most {MAX_DIMENSION} × {MAX_DIMENSION} pixels")]
    TooManyPixels,
    #[error("the picture could not be read")]
    Broken,
}

/// A picture ready to store: square, at most [`STORED_SIZE`] wide, with nothing but its pixels.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
    /// The same picture as a Face PNG, for the `Face:` header.
    pub face: Vec<u8>,
}

/// The format of an upload, by its first bytes only: never by a name or a type a browser claimed.
fn format_of(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', ..] => Some(ImageFormat::Png),
        [0xff, 0xd8, 0xff, ..] => Some(ImageFormat::Jpeg),
        [b'G', b'I', b'F', b'8', ..] => Some(ImageFormat::Gif),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::WebP),
        _ => None,
    }
}

/// Decodes a picture of a known format within the pixel and memory limits, turned the way the
/// camera said it was held. A GIF gives its first frame.
fn decode(bytes: &[u8], max_dimension: u32) -> Result<DynamicImage, PictureError> {
    let format = format_of(bytes).ok_or(PictureError::NotAPicture)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(max_dimension);
    limits.max_image_height = Some(max_dimension);
    limits.max_alloc = Some(MAX_DECODER_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|err| decode_error(&err))?;
    let orientation = decoder.orientation().ok();
    let mut picture = DynamicImage::from_decoder(decoder).map_err(|err| decode_error(&err))?;
    if let Some(orientation) = orientation {
        picture.apply_orientation(orientation);
    }
    Ok(picture)
}

fn decode_error(err: &image::ImageError) -> PictureError {
    match err {
        image::ImageError::Limits(_) => PictureError::TooManyPixels,
        image::ImageError::Unsupported(_) => PictureError::NotAPicture,
        _ => PictureError::Broken,
    }
}

/// The square in the middle of the picture.
fn centre_square(picture: DynamicImage) -> DynamicImage {
    let (width, height) = (picture.width(), picture.height());
    let side = width.min(height);
    if width == height {
        return picture;
    }
    picture.crop_imm((width - side) / 2, (height - side) / 2, side, side)
}

/// Whether any pixel is not fully opaque; only then is a PNG worth its size.
fn has_transparency(picture: &DynamicImage) -> bool {
    picture.color().has_alpha() && picture.to_rgba8().pixels().any(|pixel| pixel.0[3] < 255)
}

/// Pictures decoded at once, for the whole server: each may take a few hundred MB while it is read.
static DECODING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// [`prepare`] on the blocking pool, at most two at a time.
pub async fn prepare_upload(bytes: Vec<u8>) -> Result<Prepared, PictureError> {
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(PictureError::TooLarge);
    }
    let _permit = DECODING.acquire().await.map_err(|_| PictureError::Broken)?;
    tokio::task::spawn_blocking(move || prepare(&bytes)).await.map_err(|_| PictureError::Broken)?
}

/// Turns an upload into the picture that is stored: decoded, the centre square cut out, scaled to
/// at most [`STORED_SIZE`] and written anew — JPEG, or PNG when it has see-through parts. CPU-bound:
/// callers run it on the blocking pool.
pub fn prepare(bytes: &[u8]) -> Result<Prepared, PictureError> {
    if bytes.is_empty() {
        return Err(PictureError::Empty);
    }
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(PictureError::TooLarge);
    }
    let square = centre_square(decode(bytes, MAX_DIMENSION)?);
    let square = if square.width() > STORED_SIZE {
        square.resize_exact(STORED_SIZE, STORED_SIZE, FilterType::Lanczos3)
    } else {
        square
    };
    let face = face_png(&square);
    let (bytes, media_type) = if has_transparency(&square) {
        (encode_png(&DynamicImage::ImageRgba8(square.to_rgba8()))?, "image/png")
    } else {
        (encode_jpeg(&square.to_rgb8())?, "image/jpeg")
    };
    Ok(Prepared { bytes, media_type, face })
}

fn encode_png(picture: &DynamicImage) -> Result<Vec<u8>, PictureError> {
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Best,
        image::codecs::png::FilterType::Adaptive,
    );
    picture.write_with_encoder(encoder).map_err(|_| PictureError::Broken)?;
    Ok(out)
}

fn encode_jpeg(picture: &RgbImage) -> Result<Vec<u8>, PictureError> {
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    picture.write_with_encoder(encoder).map_err(|_| PictureError::Broken)?;
    Ok(out)
}

/// A stored picture at another size, for Libravatar's `s=`: scaled down from the stored one, never
/// up. `None` when it cannot be read (it was written here, so that does not happen).
pub fn scaled(bytes: &[u8], size: u32) -> Option<(Vec<u8>, &'static str)> {
    let picture = decode(bytes, STORED_SIZE * 2).ok()?;
    if picture.width() <= size {
        let media_type = if format_of(bytes) == Some(ImageFormat::Png) { "image/png" } else { "image/jpeg" };
        return Some((bytes.to_vec(), media_type));
    }
    let smaller = picture.resize_exact(size, size, FilterType::Triangle);
    if has_transparency(&smaller) {
        Some((encode_png(&DynamicImage::ImageRgba8(smaller.to_rgba8())).ok()?, "image/png"))
    } else {
        Some((encode_jpeg(&smaller.to_rgb8()).ok()?, "image/jpeg"))
    }
}

/// The Face PNG of a square picture: 48 × 48 on white, with a palette cut down until the file
/// takes at most [`MAX_FACE_BYTES`].
pub fn face_png(square: &DynamicImage) -> Vec<u8> {
    let small = square.resize_exact(FACE_SIZE, FACE_SIZE, FilterType::Lanczos3).to_rgba8();
    let pixels: Vec<[u8; 3]> = small.pixels().map(|pixel| on_white(pixel.0)).collect();
    let mut smallest = Vec::new();
    for colours in [256, 128, 64, 32, 16, 8, 4, 2] {
        let (palette, indices) = median_cut(&pixels, colours);
        let Some(png) = indexed_png(FACE_SIZE, &palette, &indices) else { continue };
        if png.len() <= MAX_FACE_BYTES {
            return png;
        }
        smallest = png;
    }
    // Two colours of 48 × 48 are 288 bytes before compression; this is not reached.
    smallest
}

fn on_white([r, g, b, a]: [u8; 4]) -> [u8; 3] {
    let blend = |c: u8| ((u16::from(c) * u16::from(a) + 255 * (255 - u16::from(a))) / 255) as u8;
    [blend(r), blend(g), blend(b)]
}

/// Median cut: the box of colours with the widest spread is halved at its median until there are
/// `colours` boxes; each box's average is one palette entry.
fn median_cut(pixels: &[[u8; 3]], colours: usize) -> (Vec<[u8; 3]>, Vec<u8>) {
    let mut boxes: Vec<Vec<usize>> = vec![(0..pixels.len()).collect()];
    let spread = |members: &[usize]| -> (usize, u8) {
        (0..3)
            .map(|channel| {
                let values = members.iter().map(|&i| pixels[i][channel]);
                let (low, high) = values.fold((255u8, 0u8), |(lo, hi), v| (lo.min(v), hi.max(v)));
                (channel, high.saturating_sub(low))
            })
            .max_by_key(|(_, range)| *range)
            .unwrap_or((0, 0))
    };
    while boxes.len() < colours {
        let Some((widest, (channel, range))) = boxes
            .iter()
            .enumerate()
            .filter(|(_, members)| members.len() > 1)
            .map(|(index, members)| (index, spread(members)))
            .max_by_key(|(_, (_, range))| *range)
        else {
            break;
        };
        if range == 0 {
            break;
        }
        let mut members = boxes.swap_remove(widest);
        members.sort_unstable_by_key(|&i| pixels[i][channel]);
        let upper = members.split_off(members.len() / 2);
        boxes.push(members);
        boxes.push(upper);
    }
    let mut palette = Vec::with_capacity(boxes.len());
    let mut indices = vec![0u8; pixels.len()];
    for (index, members) in boxes.iter().enumerate() {
        let mut sum = [0u32; 3];
        for &i in members {
            for channel in 0..3 {
                sum[channel] += u32::from(pixels[i][channel]);
            }
            indices[i] = index as u8;
        }
        let count = members.len().max(1) as u32;
        palette.push([(sum[0] / count) as u8, (sum[1] / count) as u8, (sum[2] / count) as u8]);
    }
    (palette, indices)
}

/// A square palette PNG with as few bits per pixel as the palette needs.
fn indexed_png(size: u32, palette: &[[u8; 3]], indices: &[u8]) -> Option<Vec<u8>> {
    let depth = match palette.len() {
        0..=2 => png::BitDepth::One,
        3..=4 => png::BitDepth::Two,
        5..=16 => png::BitDepth::Four,
        _ => png::BitDepth::Eight,
    };
    let bits = depth as usize;
    let per_row = (size as usize * bits).div_ceil(8);
    let mut data = vec![0u8; per_row * size as usize];
    for (at, &index) in indices.iter().enumerate() {
        let (row, column) = (at / size as usize, at % size as usize);
        let bit = column * bits;
        let shift = 8 - bits - bit % 8;
        data[row * per_row + bit / 8] |= index << shift;
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, size, size);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(depth);
    encoder.set_palette(palette.iter().flatten().copied().collect::<Vec<u8>>());
    encoder.set_compression(png::Compression::High);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&data).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/// A Face that came with a message, checked: a PNG of at most about 48 × 48 that decodes, and small.
/// Returns the PNG as it came.
pub fn incoming_face(value: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let compact: String = value.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if compact.len() > MAX_INCOMING_FACE_BYTES * 4 / 3 + 4 {
        return None;
    }
    let png = base64::engine::general_purpose::STANDARD.decode(compact.as_bytes()).ok()?;
    if png.len() > MAX_INCOMING_FACE_BYTES || format_of(&png) != Some(ImageFormat::Png) {
        return None;
    }
    let picture = decode(&png, 64).ok()?;
    (picture.width() >= 8 && picture.height() >= 8).then_some(png)
}

/// The `Face:` header for a Face PNG: base64, folded to lines of at most 78 characters.
pub fn face_header(png: &[u8]) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    let mut header = String::from("Face:");
    let mut line = header.len();
    for chunk in encoded.as_bytes().chunks(72) {
        if line + 1 + chunk.len() > 78 {
            header.push_str("\r\n");
            line = 0;
        }
        header.push(' ');
        header.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        line += 1 + chunk.len();
    }
    header.push_str("\r\n");
    header
}

/// The type of a picture in a vCard (`data:` or base64) by its first bytes, before it is handed
/// out: PNG, JPEG, GIF or WebP. Anything else, SVG included, is not passed on.
pub fn contact_photo_type(bytes: &[u8]) -> Option<&'static str> {
    let media_type = match format_of(bytes)? {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::WebP => "image/webp",
        _ => return None,
    };
    Some(media_type)
}

/// The generic silhouette Libravatar hands out for `d=mm`: a grey head and shoulders on a light
/// ground, as a PNG of `size` × `size`.
pub fn silhouette(size: u32) -> Vec<u8> {
    let size = size.clamp(1, STORED_SIZE);
    let s = size as f32;
    let picture = RgbaImage::from_fn(size, size, |x, y| {
        let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
        let head = (x - s * 0.5).powi(2) + (y - s * 0.38).powi(2) <= (s * 0.19).powi(2);
        let shoulders = (x - s * 0.5).powi(2) / (s * 0.34).powi(2) + (y - s * 0.95).powi(2) / (s * 0.36).powi(2) <= 1.0;
        if head || shoulders { image::Rgba([160, 160, 166, 255]) } else { image::Rgba([222, 222, 226, 255]) }
    });
    encode_png(&DynamicImage::ImageRgba8(picture)).unwrap_or_default()
}

/// A fully transparent PNG of `size` × `size`, for Libravatar's `d=blank`.
pub fn blank(size: u32) -> Vec<u8> {
    let size = size.clamp(1, STORED_SIZE);
    encode_png(&DynamicImage::ImageRgba8(RgbaImage::new(size, size))).unwrap_or_default()
}

/// A test picture of the given size in `png`, `jpeg`, `gif` or `webp`, for tests here and in the
/// other crates.
#[doc(hidden)]
pub fn sample(width: u32, height: u32, format: &str) -> Vec<u8> {
    let format = match format {
        "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::WebP,
        _ => ImageFormat::Png,
    };
    let picture = RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x * 255 / width.max(1)) as u8, (y * 255 / height.max(1)) as u8, 160, 255])
    });
    let mut out = Cursor::new(Vec::new());
    let picture = DynamicImage::ImageRgba8(picture);
    let picture = if format == ImageFormat::Jpeg { DynamicImage::ImageRgb8(picture.to_rgb8()) } else { picture };
    picture.write_to(&mut out, format).expect("a test picture can always be written");
    out.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploads_become_squares_of_at_most_512() {
        let prepared = prepare(&sample(900, 600, "png")).unwrap();
        assert_eq!(prepared.media_type, "image/jpeg", "no transparency, so JPEG");
        let stored = image::load_from_memory(&prepared.bytes).unwrap();
        assert_eq!((stored.width(), stored.height()), (512, 512));

        let small = prepare(&sample(100, 140, "jpeg")).unwrap();
        let stored = image::load_from_memory(&small.bytes).unwrap();
        assert_eq!((stored.width(), stored.height()), (100, 100), "never scaled up");

        for format in ["gif", "webp"] {
            assert!(prepare(&sample(64, 64, format)).is_ok(), "{format:?}");
        }
    }

    #[test]
    fn see_through_pictures_stay_png() {
        let picture = RgbaImage::from_fn(40, 40, |x, _| image::Rgba([255, 0, 0, if x < 20 { 0 } else { 255 }]));
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(picture).write_to(&mut png, ImageFormat::Png).unwrap();
        assert_eq!(prepare(png.get_ref()).unwrap().media_type, "image/png");
    }

    #[test]
    fn anything_else_is_refused() {
        assert_eq!(prepare(b"").unwrap_err(), PictureError::Empty);
        assert_eq!(prepare(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap_err(), PictureError::NotAPicture);
        assert_eq!(prepare(b"\x89PNG\r\n\x1a\nnot really").unwrap_err(), PictureError::Broken);
        assert_eq!(prepare(&vec![0; MAX_UPLOAD_BYTES + 1]).unwrap_err(), PictureError::TooLarge);
    }

    /// A PNG that claims 9000 × 9000 is turned away before anything is allocated for it.
    #[test]
    fn huge_pictures_are_refused_by_their_header() {
        let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        header.extend_from_slice(&9000u32.to_be_bytes());
        header.extend_from_slice(&9000u32.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        let crc = {
            let mut crc = 0xffff_ffffu32;
            for &byte in &header[12..] {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
                }
            }
            !crc
        };
        header.extend_from_slice(&crc.to_be_bytes());
        assert_eq!(prepare(&header).unwrap_err(), PictureError::TooManyPixels);
    }

    #[test]
    fn metadata_does_not_survive() {
        let mut jpeg = sample(64, 64, "jpeg");
        // An APP1 (Exif) segment right after the start marker.
        let exif = b"\xff\xe1\x00\x16Exif\0\0secret-camera-x";
        jpeg.splice(2..2, exif.iter().copied());
        let prepared = prepare(&jpeg).unwrap();
        assert!(!prepared.bytes.windows(13).any(|w| w == b"secret-camera"));
    }

    #[test]
    fn faces_fit_the_header() {
        // A noisy picture needs fewer colours before it fits.
        let noisy = RgbaImage::from_fn(300, 300, |x, y| {
            let n = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) as u8;
            image::Rgba([n, n.wrapping_mul(7), n.wrapping_mul(13), 255])
        });
        for picture in [DynamicImage::ImageRgba8(noisy), image::load_from_memory(&sample(300, 300, "png")).unwrap()] {
            let face = face_png(&picture);
            assert!(face.len() <= MAX_FACE_BYTES, "{} bytes", face.len());
            let decoded = image::load_from_memory(&face).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (48, 48));
            let header = face_header(&face);
            assert!(header.lines().all(|line| line.len() <= 78));
            let value = header.strip_prefix("Face:").unwrap();
            assert_eq!(incoming_face(value).unwrap(), face);
        }
    }

    #[test]
    fn incoming_faces_are_checked() {
        assert!(incoming_face("not base64 at all!").is_none());
        use base64::Engine;
        let big = base64::engine::general_purpose::STANDARD.encode(sample(200, 200, "png"));
        assert!(incoming_face(&big).is_none(), "too many pixels or bytes");
        let jpeg = base64::engine::general_purpose::STANDARD.encode(sample(48, 48, "jpeg"));
        assert!(incoming_face(&jpeg).is_none(), "only PNG");
    }

    #[test]
    fn stored_pictures_are_scaled_for_libravatar() {
        let prepared = prepare(&sample(600, 600, "png")).unwrap();
        let (small, media_type) = scaled(&prepared.bytes, 80).unwrap();
        assert_eq!(media_type, "image/jpeg");
        assert_eq!(image::load_from_memory(&small).unwrap().width(), 80);
        let (same, _) = scaled(&prepared.bytes, 512).unwrap();
        assert_eq!(same, prepared.bytes);
    }
}
