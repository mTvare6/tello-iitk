use super::neural::Onnx;
use image::GrayImage;

const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

pub struct Network {
    model: Onnx,
}

pub struct Prediction {
    pub text: String,
    pub confidence: f32,
}

/// Keep the aspect ratio and detached parts of the letter.
fn normalize(mask: &GrayImage) -> Option<Vec<u8>> {
    let (width, height) = mask.dimensions();

    if width == 0 || height == 0 {
        return None;
    }

    let (out_w, out_h) = if width >= height {
        (26, (height * 26 + width / 2) / width)
    } else {
        ((width * 26 + height / 2) / height, 26)
    };

    let (out_w, out_h) = (out_w.max(1), out_h.max(1));
    let (left, top) = ((32 - out_w) / 2, (32 - out_h) / 2);

    let mut output = vec![0; 32 * 32];

    for y in 0..out_h {
        for x in 0..out_w {
            let sx = ((2 * x + 1) * width / (2 * out_w)).min(width - 1);
            let sy = ((2 * y + 1) * height / (2 * out_h)).min(height - 1);
            output[((top + y) * 32 + left + x) as usize] = 255 - mask.get_pixel(sx, sy)[0];
        }
    }

    Some(output)
}

impl Network {
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(Self {
            model: Onnx::load(bytes, &[1, 1, 32, 32])?,
        })
    }

    fn infer_ink(&self, ink: &[u8]) -> anyhow::Result<Vec<f32>> {
        self.model.run(ink, &[1, 1, 32, 32])
    }

    fn predict_glyph(&self, mask: &GrayImage) -> Option<(char, f32)> {
        let logits = self.infer_ink(&normalize(mask)?).ok()?;
        let best = (1..logits.len()).fold(0, |a, b| if logits[b] > logits[a] { b } else { a });
        let denominator: f32 = logits.iter().map(|&n| (n - logits[best]).exp()).sum();

        Some((ALPHABET[best] as char, 1.0 / denominator))
    }

    pub(super) fn predict_crops(&self, glyphs: &[GrayImage]) -> Option<Prediction> {
        if glyphs.len() != 5 {
            return None;
        }

        let mut text = String::with_capacity(5);
        let mut confidence = 1.0_f32;

        for glyph in glyphs {
            let (c, score) = self.predict_glyph(glyph)?;
            text.push(c);
            confidence = confidence.min(score);
        }

        Some(Prediction { text, confidence })
    }
}
