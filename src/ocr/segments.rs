use image::{GrayImage, Luma};

pub(super) fn split_letters(mask: &GrayImage) -> Vec<GrayImage> {
    split_with_gap(mask, 3, true)
}

fn split_with_gap(mask: &GrayImage, max_gap: u32, expand: bool) -> Vec<GrayImage> {
    let runs = column_runs(mask, max_gap);

    runs.into_iter()
        .filter_map(|(left, right)| {
            if right - left < 2 {
                return None;
            }

            let left = if expand { left.saturating_sub(2) } else { left };

            let right = if expand {
                (right + 2).min(mask.width())
            } else {
                right
            };

            let (mut min_x, mut min_y, mut max_x, mut max_y) = (right, mask.height(), 0, 0);

            for y in 0..mask.height() {
                for x in left..right {
                    if mask.get_pixel(x, y)[0] == 0 {
                        min_x = min_x.min(x);
                        min_y = min_y.min(y);
                        max_x = max_x.max(x);
                        max_y = max_y.max(y);
                    }
                }
            }

            if min_x > max_x || min_y > max_y {
                return None;
            }

            Some(
                image::imageops::crop_imm(mask, min_x, min_y, max_x - min_x + 1, max_y - min_y + 1)
                    .to_image(),
            )
        })
        .collect()
}

fn column_runs(mask: &GrayImage, max_gap: u32) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();

    for x in 0..mask.width() {
        if (0..mask.height()).any(|y| mask.get_pixel(x, y)[0] == 0) {
            if let Some(last) = runs.last_mut()
                && x - last.1 <= max_gap
            {
                last.1 = x + 1;
            } else {
                runs.push((x, x + 1));
            }
        }
    }

    runs.retain(|(left, right)| right - left >= 2);

    runs
}

fn crop_spans(mask: &GrayImage, spans: &[(u32, u32)]) -> Option<Vec<GrayImage>> {
    spans
        .iter()
        .map(|&(left, right)| {
            let (mut x0, mut y0, mut x1, mut y1) = (right, mask.height(), 0, 0);

            for y in 0..mask.height() {
                for x in left..right {
                    if mask.get_pixel(x, y)[0] == 0 {
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x);
                        y1 = y1.max(y);
                    }
                }
            }

            if x0 > x1 || y0 > y1 {
                return None;
            }

            Some(image::imageops::crop_imm(mask, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image())
        })
        .collect()
}

fn split_touching(mask: &GrayImage, spans: Vec<(u32, u32)>, out: &mut Vec<Vec<GrayImage>>) {
    if spans.len() == 5 {
        if let Some(crops) = crop_spans(mask, &spans) {
            out.push(crops);
        }

        return;
    }

    if !(3..5).contains(&spans.len()) {
        return;
    }

    for (index, &(left, right)) in spans.iter().enumerate() {
        if right - left < 30 {
            continue;
        }

        let mut cuts: Vec<_> = (left + 8..right - 8)
            .map(|x| {
                let ink = (0..mask.height())
                    .filter(|&y| mask.get_pixel(x, y)[0] == 0)
                    .count();

                (ink, x)
            })
            .collect();

        cuts.sort_unstable();

        let mut selected = Vec::new();

        for (_, x) in cuts {
            if selected.iter().all(|&old: &u32| old.abs_diff(x) >= 4) {
                selected.push(x);
            }

            if selected.len() == 5 {
                break;
            }
        }

        for cut in selected {
            let mut next = spans.clone();
            next.splice(index..=index, [(left, cut), (cut, right)]);
            split_touching(mask, next, out);
        }
    }
}

/// Remove borders when almost the whole edge is dark.
pub(super) fn clean_frame(mut mask: GrayImage) -> GrayImage {
    let (w, h) = mask.dimensions();

    if w == 0 || h == 0 {
        return mask;
    }

    for y in [0, h - 1] {
        if (0..w).filter(|&x| mask.get_pixel(x, y)[0] == 0).count() * 10 >= w as usize * 9 {
            for x in 0..w {
                mask.put_pixel(x, y, Luma([255]));
            }
        }
    }

    for x in [0, w - 1] {
        if (0..h).filter(|&y| mask.get_pixel(x, y)[0] == 0).count() * 10 >= h as usize * 8 {
            for y in 0..h {
                mask.put_pixel(x, y, Luma([255]));
            }
        }
    }

    mask
}

pub(super) fn segment_candidates(mask: &GrayImage) -> Vec<Vec<GrayImage>> {
    let baseline = split_letters(mask);

    if baseline.len() == 5 {
        return vec![baseline];
    }

    let mut candidates = Vec::new();

    for gap in [0, 1, 2, 4, 5, 6] {
        let letters = split_with_gap(mask, gap, false);

        if letters.len() == 5 && !candidates.contains(&letters) {
            candidates.push(letters);
        }
    }

    if candidates.is_empty() && mask.height() >= 12 {
        split_touching(mask, column_runs(mask, 0), &mut candidates);
    }

    if candidates.is_empty()
        && let Some(letters) = component_candidate(mask)
    {
        candidates.push(letters);
    }

    candidates
}

#[derive(Debug)]
struct Component {
    points: Vec<(u32, u32)>,
    left: u32,
    right: u32,
    top: u32,
    bottom: u32,
}

/// Split overlapping letters by their connected pieces.
fn component_candidate(mask: &GrayImage) -> Option<Vec<GrayImage>> {
    let (width, height) = mask.dimensions();

    if width < 5 || height < 12 {
        return None;
    }

    let mut visited = vec![false; (width * height) as usize];
    let mut components = Vec::<Component>::new();

    for y in 0..height {
        for x in 0..width {
            let index = (y * width + x) as usize;

            if visited[index] || mask.get_pixel(x, y)[0] != 0 {
                continue;
            }

            visited[index] = true;

            let mut stack = vec![(x, y)];

            let mut component = Component {
                points: Vec::new(),
                left: x,
                right: x,
                top: y,
                bottom: y,
            };

            while let Some((px, py)) = stack.pop() {
                component.points.push((px, py));
                component.left = component.left.min(px);
                component.right = component.right.max(px);
                component.top = component.top.min(py);
                component.bottom = component.bottom.max(py);

                for ny in py.saturating_sub(1)..=(py + 1).min(height - 1) {
                    for nx in px.saturating_sub(1)..=(px + 1).min(width - 1) {
                        let index = (ny * width + nx) as usize;

                        if !visited[index] && mask.get_pixel(nx, ny)[0] == 0 {
                            visited[index] = true;
                            stack.push((nx, ny));
                        }
                    }
                }
            }

            // Single pixels are usually noise. Keep the larger dots.
            if component.points.len() >= 2 {
                components.push(component);
            }
        }
    }

    // Too many components make the pair search expensive.
    if components.len() < 5 || components.len() > 32 {
        return None;
    }

    while components.len() > 5 {
        let mut best: Option<(u32, usize, usize)> = None;

        for i in 0..components.len() {
            for j in i + 1..components.len() {
                let (a, b) = (&components[i], &components[j]);
                let dx = a
                    .left
                    .saturating_sub(b.right)
                    .max(b.left.saturating_sub(a.right));
                let dy = a
                    .top
                    .saturating_sub(b.bottom)
                    .max(b.top.saturating_sub(a.bottom));

                // Merge pieces with similar x positions and a small vertical gap.
                if dx > 2 || dy > height / 2 {
                    continue;
                }

                let score = dx * height + dy;

                if best.is_none_or(|old| score < old.0) {
                    best = Some((score, i, j));
                }
            }
        }

        let (_, i, j) = best?;
        let b = components.remove(j);
        let a = &mut components[i];
        a.left = a.left.min(b.left);
        a.right = a.right.max(b.right);
        a.top = a.top.min(b.top);
        a.bottom = a.bottom.max(b.bottom);
        a.points.extend(b.points);
    }

    components.sort_by_key(|c| (c.left + c.right, c.left));

    Some(
        components
            .into_iter()
            .map(|component| {
                let mut crop = GrayImage::from_pixel(
                    component.right - component.left + 1,
                    component.bottom - component.top + 1,
                    Luma([255]),
                );

                for (x, y) in component.points {
                    crop.put_pixel(x - component.left, y - component.top, Luma([0]));
                }

                crop
            })
            .collect(),
    )
}
