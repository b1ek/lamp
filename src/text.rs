use crate::canvas::{Canvas, Color};
use fontdue::{Font, FontSettings};
use std::collections::HashMap;

pub struct Text {
    font: Font,
    glyphs: HashMap<GlyphKey, Glyph>,
}

#[derive(PartialEq, Eq, Hash, Clone, Copy)]
struct GlyphKey {
    character: char,
    quantized_size: u32,
}

struct Glyph {
    metrics: fontdue::Metrics,
    coverage: Vec<u8>,
}

impl Text {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let font = Font::from_bytes(bytes, FontSettings::default()).unwrap();

        Self {
            font,
            glyphs: HashMap::new(),
        }
    }

    pub fn ascent(&self, size: f32) -> f32 {
        self.font
            .horizontal_line_metrics(size)
            .map(|metrics| metrics.ascent)
            .unwrap_or(size * 0.8)
    }

    pub fn descent(&self, size: f32) -> f32 {
        self.font
            .horizontal_line_metrics(size)
            .map(|metrics| metrics.descent)
            .unwrap_or(-size * 0.2)
    }

    pub fn baseline(&self, line_top: f32, line_height: f32, size: f32) -> f32 {
        let glyph_height = self.ascent(size) - self.descent(size);
        line_top + (line_height - glyph_height) / 2.0 + self.ascent(size)
    }

    pub fn run_width(&mut self, run: &str, size: f32, letter_spacing: f32) -> f32 {
        let mut width = 0.0;
        let mut glyph_count: i32 = 0;

        for character in run.chars() {
            width += self.glyph(character, size).metrics.advance_width;
            glyph_count += 1;
        }

        let spacing = letter_spacing * glyph_count.saturating_sub(1) as f32;

        width + spacing
    }

    pub fn draw_run(
        &mut self,
        canvas: &mut Canvas,
        run: &str,
        pen_x: f32,
        baseline: f32,
        size: f32,
        letter_spacing: f32,
        color: Color,
    ) -> f32 {
        let mut pen = pen_x;

        for character in run.chars() {
            let glyph = self.glyph(character, size);
            let bitmap_top = baseline - (glyph.metrics.ymin + glyph.metrics.height as i32) as f32;
            let bitmap_left = pen + glyph.metrics.xmin as f32;

            for row in 0..glyph.metrics.height {
                for column in 0..glyph.metrics.width {
                    let coverage = glyph.coverage[row * glyph.metrics.width + column];

                    if coverage > 0 {
                        let x = bitmap_left.round() as i32 + column as i32;
                        let y = bitmap_top.round() as i32 + row as i32;
                        canvas.blend(x, y, color, coverage as f32 / 255.0);
                    }
                }
            }

            pen += glyph.metrics.advance_width + letter_spacing;
        }

        pen - letter_spacing
    }

    fn glyph(&mut self, character: char, size: f32) -> &Glyph {
        let key = GlyphKey {
            character,
            quantized_size: (size * 16.0) as u32,
        };

        self.glyphs.entry(key).or_insert_with(|| {
            let (metrics, coverage) = self.font.rasterize(character, size);

            Glyph { metrics, coverage }
        })
    }
}
