use crate::PlayerError;

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Largest skin file accepted.
pub const MAX_SKIN_BYTES: usize = 256 * 1024;

/// Checks that `png` is a skin image: a PNG of 64x64 pixels, or the old
/// 64x32 format, and returns its width and height.
///
/// # Errors
///
/// Returns [`PlayerError::InvalidSkin`] for anything else.
///
/// # Examples
///
/// ```
/// let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
/// png.extend_from_slice(&64_u32.to_be_bytes());
/// png.extend_from_slice(&32_u32.to_be_bytes());
/// assert_eq!(coblet_player::check_skin_png(&png)?, (64, 32));
/// # Ok::<(), coblet_player::PlayerError>(())
/// ```
pub fn check_skin_png(png: &[u8]) -> Result<(u32, u32), PlayerError> {
    let invalid = |message: &str| PlayerError::InvalidSkin(message.to_owned());
    if png.len() > MAX_SKIN_BYTES {
        return Err(invalid("the file is larger than 256 KiB"));
    }
    if !png.starts_with(SIGNATURE) {
        return Err(invalid("the file is not a png image"));
    }
    if png.get(12..16) != Some(b"IHDR".as_slice()) {
        return Err(invalid("the png has no header chunk"));
    }
    let number = |range: std::ops::Range<usize>| {
        png.get(range)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_be_bytes)
    };
    let (Some(width), Some(height)) = (number(16..20), number(20..24)) else {
        return Err(invalid("the png header is cut off"));
    };
    match (width, height) {
        (64, 64 | 32) => Ok((width, height)),
        _ => Err(PlayerError::InvalidSkin(format!(
            "the image is {width}x{height}, skins are 64x64 or 64x32"
        ))),
    }
}

#[cfg(test)]
pub(crate) fn sample(width: u32, height: u32) -> Vec<u8> {
    let mut png = SIGNATURE.to_vec();
    png.extend_from_slice(&13_u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&width.to_be_bytes());
    png.extend_from_slice(&height.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    png
}

#[cfg(test)]
#[path = "../tests/png.rs"]
mod tests;
