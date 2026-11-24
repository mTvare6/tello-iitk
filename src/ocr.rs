use image::{GrayImage, ImageBuffer, Luma};
use std::io::Cursor;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;

#[cfg(not(target_arch = "wasm32"))]
mod glyph_model;
#[cfg(not(target_arch = "wasm32"))]
mod neural;
mod segments;

use segments::{clean_frame, segment_candidates};

pub struct Prediction {
    pub text: String,
    pub confidence: f64,
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn suggest_code(bytes: Vec<u8>) -> Option<Prediction> {
    if std::env::var("HELLO_CAPTCHA_OCR").as_deref() == Ok("0") {
        return None;
    }

    tokio::task::spawn_blocking(move || recognize(&bytes))
        .await
        .ok()?
}

fn decode_mask(bytes: &[u8]) -> Option<GrayImage> {
    if bytes.len() > 2 * 1024 * 1024 {
        return None;
    }

    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;

    let mut limits = image::Limits::default();
    limits.max_image_width = Some(512);
    limits.max_image_height = Some(256);
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);

    let image = reader.decode().ok()?.to_rgb8();
    let mask = ImageBuffer::from_fn(image.width(), image.height(), |x, y| {
        let p = image.get_pixel(x, y).0;
        let gray = (p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114 + 500) / 1000;

        Luma([if gray < 200 { 0 } else { 255 }])
    });

    Some(mask)
}

const TEMPLATE_SIZE: usize = 16 * 24;
const TEMPLATE_RECORD: usize = 1 + TEMPLATE_SIZE;
const TEMPLATES: &[u8] = include_bytes!("../assets/captcha-templates.bin");

fn normalized(letter: &GrayImage) -> Vec<u8> {
    image::imageops::resize(letter, 16, 24, image::imageops::FilterType::Lanczos3).into_raw()
}

fn nearest_template(values: &[u8], templates: &[u8]) -> Option<(u64, u8)> {
    if values.len() != TEMPLATE_SIZE
        || templates.is_empty()
        || !templates.len().is_multiple_of(TEMPLATE_RECORD)
    {
        return None;
    }

    templates
        .as_chunks::<TEMPLATE_RECORD>()
        .0
        .iter()
        .map(|entry| {
            let distance = entry[1..]
                .iter()
                .zip(values)
                .map(|(&a, &b)| {
                    let d = a as i32 - b as i32;

                    (d * d) as u64
                })
                .sum::<u64>();

            (distance, entry[0])
        })
        .min_by_key(|&(distance, _)| distance)
        .filter(|(_, c)| c.is_ascii_alphanumeric())
}

#[cfg(not(target_arch = "wasm32"))]
fn recognize(bytes: &[u8]) -> Option<Prediction> {
    let mask = decode_mask(bytes)?;

    if mask.width() < 40
        || mask.height() < 20
        || mask.pixels().all(|p| p[0] == mask.get_pixel(0, 0)[0])
    {
        return None;
    }

    let clean = clean_frame(mask.clone());

    default_predict(&mask, &clean)
}

#[cfg(not(target_arch = "wasm32"))]
const CNN_BYTES: &[u8] = include_bytes!("../assets/captcha-cnn.onnx");

#[cfg(not(target_arch = "wasm32"))]
static CNN: OnceLock<Option<neural::Network>> = OnceLock::new();

#[cfg(not(target_arch = "wasm32"))]
const GLYPH_BYTES: &[u8] = include_bytes!("../assets/captcha-glyph.onnx");

#[cfg(not(target_arch = "wasm32"))]
static GLYPH_CNN: OnceLock<Option<glyph_model::Network>> = OnceLock::new();

#[cfg(not(target_arch = "wasm32"))]
fn default_predict(mask: &GrayImage, clean: &GrayImage) -> Option<Prediction> {
    if let Some(network) =
        GLYPH_CNN.get_or_init(|| glyph_model::Network::from_bytes(GLYPH_BYTES).ok())
        && let Some(crops) = select_glyphs(clean)
        && let Some(p) = network.predict_crops(&crops)
    {
        return Some(Prediction {
            text: p.text,
            confidence: p.confidence as f64,
        });
    }

    if let Some(network) = CNN.get_or_init(|| neural::Network::from_bytes(CNN_BYTES).ok())
        && let Some(p) = network.predict_mask(mask)
    {
        return Some(Prediction {
            text: p.text,
            confidence: p.confidence as f64,
        });
    }

    segment_candidates(clean)
        .iter()
        .filter_map(|candidate| {
            predict_letters(candidate).map(|p| (candidate_distance(candidate), p))
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, p)| p)
}

fn select_glyphs(mask: &GrayImage) -> Option<Vec<GrayImage>> {
    segment_candidates(mask)
        .into_iter()
        .min_by_key(|candidate| candidate_distance(candidate))
}

fn candidate_distance(letters: &[GrayImage]) -> u64 {
    letters
        .iter()
        .map(|letter| {
            nearest_template(&normalized(letter), TEMPLATES)
                .map_or(u64::MAX / 5, |(distance, _)| distance)
        })
        .sum()
}

#[cfg(not(target_arch = "wasm32"))]
fn predict_letters(letters: &[GrayImage]) -> Option<Prediction> {
    if letters.len() != 5 {
        return None;
    }

    let mut text = String::with_capacity(5);
    let mut confidence = 1.0_f64;

    for letter in letters {
        let (distance, c) = nearest_template(&normalized(letter), TEMPLATES)?;
        text.push(char::from(c));

        let similarity = 1.0 - (distance as f64 / TEMPLATE_SIZE as f64).sqrt() / 255.0;
        confidence = confidence.min(similarity);
    }

    Some(Prediction { text, confidence })
}

/// Keep the aspect ratio and detached parts of the letter.
pub(super) fn normalize_glyph(mask: &GrayImage) -> Option<Vec<u8>> {
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

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(module = "/web/ocr.js")]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(catch)]
    async fn predict(
        glyphs: js_sys::Array,
        whole: js_sys::Uint8Array,
    ) -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>;
}

#[cfg(target_arch = "wasm32")]
pub async fn suggest_code(bytes: Vec<u8>) -> Option<Prediction> {
    let mask = decode_mask(&bytes)?;
    if mask.width() < 40
        || mask.height() < 20
        || mask.pixels().all(|p| p[0] == mask.get_pixel(0, 0)[0])
    {
        return None;
    }
    let clean = clean_frame(mask.clone());
    let glyphs = js_sys::Array::new();
    if let Some(crops) = select_glyphs(&clean) {
        for crop in crops {
            glyphs.push(&js_sys::Uint8Array::from(
                normalize_glyph(&crop)?.as_slice(),
            ));
        }
    }
    let mut whole = Vec::with_capacity(64 * 160);
    for y in 0..64 {
        let sy = ((2 * y + 1) * mask.height() / 128).min(mask.height() - 1);
        for x in 0..160 {
            let sx = ((2 * x + 1) * mask.width() / 320).min(mask.width() - 1);
            whole.push(255 - mask.get_pixel(sx, sy)[0]);
        }
    }
    let result = predict(glyphs, js_sys::Uint8Array::from(whole.as_slice()))
        .await
        .ok()?;
    let text = js_sys::Reflect::get(&result, &"text".into())
        .ok()?
        .as_string()?;
    let confidence = js_sys::Reflect::get(&result, &"confidence".into())
        .ok()?
        .as_f64()?;
    if text.len() != 5
        || !text.bytes().all(|c| c.is_ascii_alphanumeric())
        || !confidence.is_finite()
    {
        return None;
    }

    Some(Prediction { text, confidence })
}
