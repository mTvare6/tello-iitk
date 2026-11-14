use anyhow::{Result, ensure};
use tract_onnx::prelude::*;

const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const INPUT_SIZE: usize = 64 * 160;

type Plan = SimplePlan<TypedFact, Box<dyn TypedOp>, TypedModel>;

pub(super) struct Onnx {
    model: Plan,
}

impl Onnx {
    pub(super) fn load(bytes: &[u8], shape: &[usize]) -> Result<Self> {
        let model = tract_onnx::onnx()
            .model_for_read(&mut std::io::Cursor::new(bytes))?
            .with_input_fact(0, f32::fact(shape).into())?
            .into_optimized()?
            .into_runnable()?;

        Ok(Self { model })
    }

    pub(super) fn run(&self, ink: &[u8], shape: &[usize]) -> Result<Vec<f32>> {
        ensure!(
            ink.len() == shape.iter().product::<usize>(),
            "Invalid input size"
        );

        let input: Vec<f32> = ink.iter().map(|&p| p as f32 / 255.0).collect();
        let tensor = Tensor::from_shape(shape, &input)?;
        let outputs = self.model.run(tvec!(tensor.into()))?;
        let logits: Vec<f32> = outputs[0].to_array_view::<f32>()?.iter().copied().collect();

        ensure!(
            logits.iter().all(|v| v.is_finite()),
            "Non-finite ONNX output"
        );

        Ok(logits)
    }
}

pub struct Network {
    model: Onnx,
}

pub struct NeuralPrediction {
    pub text: String,
    pub confidence: f32,
}

impl Network {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            model: Onnx::load(bytes, &[1, 1, 64, 160])?,
        })
    }

    fn infer_ink(&self, ink: &[u8]) -> Result<Vec<f32>> {
        self.model.run(ink, &[1, 1, 64, 160])
    }

    pub(super) fn predict_mask(&self, mask: &image::GrayImage) -> Option<NeuralPrediction> {
        let (width, height) = mask.dimensions();

        if width < 40 || height < 20 {
            return None;
        }

        let mut pixels = Vec::with_capacity(INPUT_SIZE);

        for y in 0..64 {
            let sy = ((2 * y + 1) * height / 128).min(height - 1);

            for x in 0..160 {
                let sx = ((2 * x + 1) * width / 320).min(width - 1);
                pixels.push(255 - mask.get_pixel(sx, sy)[0]);
            }
        }

        let logits = self.infer_ink(&pixels).ok()?;

        let mut text = String::with_capacity(5);
        let mut confidence = 1.0_f32;

        for row in logits.as_chunks::<62>().0 {
            let (i, &best) = row.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;

            if !best.is_finite() {
                return None;
            }

            let sum: f32 = row.iter().map(|&value| (value - best).exp()).sum();
            confidence = confidence.min(1.0 / sum);
            text.push(ALPHABET[i] as char);
        }

        Some(NeuralPrediction { text, confidence })
    }
}
