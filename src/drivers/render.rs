//! Render trait

#![allow(dead_code)]

pub trait Renderer {
    fn draw_char(&mut self, row: usize, column: usize, ch: u8, inverted: bool);

    fn fill(&mut self);

    fn scroll(&mut self);
}
