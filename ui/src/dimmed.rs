//! A draw target wrapper that greys out everything drawn through it.
//!
//! Stale pages are drawn with exactly the same code as fresh ones, just
//! through `Dimmed`, so the two can never drift apart.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

use crate::theme::dimmed;

/// Wraps a draw target and maps every colour to [`dimmed`].
pub struct Dimmed<'a, D>(pub &'a mut D);

impl<D: DrawTarget<Color = Rgb565>> DrawTarget for Dimmed<'_, D> {
    type Color = Rgb565;
    type Error = D::Error;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        self.0
            .draw_iter(pixels.into_iter().map(|Pixel(p, c)| Pixel(p, dimmed(c))))
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        self.0.fill_contiguous(area, colors.into_iter().map(dimmed))
    }

    // Pass solid fills through as fills, so they stay fast on the real panel.
    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        self.0.fill_solid(area, dimmed(color))
    }
}

impl<D: Dimensions> Dimensions for Dimmed<'_, D> {
    fn bounding_box(&self) -> Rectangle {
        self.0.bounding_box()
    }
}
