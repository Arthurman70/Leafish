//! Explicit diagnostic export from the game's own rendered framebuffer.
//! Does not capture the desktop, user input, another process or a server world.
use std::error::Error;
use std::fs::OpenOptions;
use std::path::Path;

pub fn capture(path: &Path, width: u32, height: u32) -> Result<(), Box<dyn Error>> {
    let pixels = crate::gl::read_window_rgba(width, height)?;
    let mut frame =
        image::RgbaImage::from_raw(width, height, pixels).ok_or("Invalid frame dimensions")?;
    // OpenGL's first row is at the bottom; PNG's first row is at the top.
    image::imageops::flip_vertical_in_place(&mut frame);
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    image::DynamicImage::ImageRgba8(frame).write_to(&mut file, image::ImageFormat::Png)?;
    Ok(())
}
