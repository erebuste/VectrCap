//! Exact row evidence in three independent width bands. No approximate seams.
use crate::app::error::AppError;
use image::RgbaImage;
use std::collections::HashSet;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlapResult {
    Matched { new_rows: u32 },
    Identical,
    Ambiguous,
    NoOverlap,
}
fn hashes(image: &RgbaImage, band: u32) -> Vec<u64> {
    let w = image.width();
    let left = w * band / 3;
    let right = w * (band + 1) / 3;
    (0..image.height())
        .map(|y| {
            let row = &image.as_raw()[((y as usize * w as usize + left as usize) * 4)
                ..((y as usize * w as usize + right as usize) * 4)];
            row.iter().fold(14695981039346656037u64, |h, b| {
                (h ^ u64::from(*b)).wrapping_mul(1099511628211)
            })
        })
        .collect()
}
fn distinctive(rows: &[u64]) -> bool {
    rows.iter().copied().collect::<HashSet<_>>().len() >= 8.min(rows.len())
}
pub fn match_vertical_overlap(
    previous: &RgbaImage,
    candidate: &RgbaImage,
) -> Result<OverlapResult, AppError> {
    super::validate_image(previous)?;
    super::validate_image(candidate)?;
    if previous.dimensions() != candidate.dimensions() {
        return Err(AppError::Capture("scroll viewport dimensions changed"));
    }
    let (w, h) = previous.dimensions();
    if w < 3 || h < 8 {
        return Ok(OverlapResult::Ambiguous);
    }
    let min_overlap = 8.max(h / 4);
    // Bound adversarial skinny/tall image work independently of the pixel budget.
    if u64::from(h) * u64::from(h) > 16_000_000 {
        return Ok(OverlapResult::Ambiguous);
    }
    let mut winners = Vec::new();
    let mut any_evidence = false;
    for band in 0..3 {
        let a = hashes(previous, band);
        let b = hashes(candidate, band);
        let mut plausible = Vec::new();
        for shift in 0..=h - min_overlap {
            let len = (h - shift) as usize;
            let equal = a[shift as usize..]
                .iter()
                .zip(&b[..len])
                .filter(|(x, y)| x == y)
                .count();
            if equal * 100 >= len * 95 {
                any_evidence = true;
                plausible.push((
                    shift,
                    equal == len && distinctive(&a[shift as usize..]) && distinctive(&b[..len]),
                ));
            }
        }
        if plausible.len() != 1 || !plausible[0].1 {
            winners.push(None);
        } else {
            winners.push(Some(plausible[0].0));
        }
    }
    if winners.iter().any(Option::is_none) || winners.windows(2).any(|w| w[0] != w[1]) {
        return Ok(if any_evidence {
            OverlapResult::Ambiguous
        } else {
            OverlapResult::NoOverlap
        });
    }
    let shift = winners[0].expect("unique band evidence");
    let row = w as usize * 4;
    let offset = shift as usize * row;
    let len = (h - shift) as usize * row;
    if previous.as_raw()[offset..] != candidate.as_raw()[..len] {
        return Ok(OverlapResult::Ambiguous);
    }
    Ok(if shift == 0 {
        OverlapResult::Identical
    } else {
        OverlapResult::Matched { new_rows: shift }
    })
}
