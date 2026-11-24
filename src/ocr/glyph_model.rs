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

use super::normalize_glyph;

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
        let logits = self.infer_ink(&normalize_glyph(mask)?).ok()?;
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
