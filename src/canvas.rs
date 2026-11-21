use core_maths::CoreFloat;

pub type Color = [u8; 3];

#[derive(Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

pub fn difference(base: Color, ink: Color) -> Color {
    [0, 1, 2].map(|index| base[index].abs_diff(ink[index]))
}

pub struct Canvas<'a> {
    pub pixels: &'a mut [u32],
    pub width: i32,
    pub height: i32,
}

impl<'a> Canvas<'a> {
    pub fn new(pixels: &'a mut [u32], width: i32, height: i32) -> Self {
        Self { pixels, width, height }
    }

    pub fn clear(&mut self, color: Color) {
        let packed = pack(color);
        self.pixels.fill(packed);
    }

    pub fn blend(&mut self, x: i32, y: i32, color: Color, coverage: f32) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }

        if coverage <= 0.0 {
            return;
        }

        let index = (y * self.width + x) as usize;
        let destination = self.pixels[index];

        if coverage >= 1.0 {
            self.pixels[index] = pack(color);
            return;
        }

        let mut red = ((destination >> 16) & 0xff) as f32;
        let mut green = ((destination >> 8) & 0xff) as f32;
        let mut blue = (destination & 0xff) as f32;

        red += (color[0] as f32 - red) * coverage;
        green += (color[1] as f32 - green) * coverage;
        blue += (color[2] as f32 - blue) * coverage;

        self.pixels[index] = (red as u32) << 16 | (green as u32) << 8 | blue as u32;
    }

    pub fn fill_shaped(&mut self, bounds: Rect, color: Color, inside: impl Fn(f32, f32) -> bool) {
        const SAMPLES_PER_AXIS: i32 = 4;
        const TOTAL_SAMPLES: f32 = (SAMPLES_PER_AXIS * SAMPLES_PER_AXIS) as f32;

        let left = bounds.x.floor() as i32;
        let top = bounds.y.floor() as i32;
        let right = bounds.right().ceil() as i32;
        let bottom = bounds.bottom().ceil() as i32;

        for y in top..bottom {
            for x in left..right {
                let mut hits = 0;

                for sample_y in 0..SAMPLES_PER_AXIS {
                    for sample_x in 0..SAMPLES_PER_AXIS {
                        let position_x = x as f32 + (sample_x as f32 + 0.5) / SAMPLES_PER_AXIS as f32;
                        let position_y = y as f32 + (sample_y as f32 + 0.5) / SAMPLES_PER_AXIS as f32;

                        if inside(position_x, position_y) {
                            hits += 1;
                        }
                    }
                }

                if hits > 0 {
                    self.blend(x, y, color, hits as f32 / TOTAL_SAMPLES);
                }
            }
        }
    }
}

fn pack(color: Color) -> u32 {
    (color[0] as u32) << 16 | (color[1] as u32) << 8 | color[2] as u32
}
