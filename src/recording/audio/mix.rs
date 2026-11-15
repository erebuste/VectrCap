use super::MAX_AUDIO_FRAMES;
use crate::{
    app::error::AppError,
    recording::options::{AudioMode, AudioOptions},
};
pub fn mix_stereo(
    system: Option<&[f32]>,
    microphone: Option<&[f32]>,
    o: &AudioOptions,
) -> Result<Vec<i16>, AppError> {
    o.validate()?;
    let expected = match o.mode {
        AudioMode::Off => return Err(AppError::Capture("audio mixing is disabled")),
        AudioMode::System => (true, false),
        AudioMode::Microphone => (false, true),
        AudioMode::Both => (true, true),
    };
    if (system.is_some(), microphone.is_some()) != expected {
        return Err(AppError::Capture("selected audio source missing from mix"));
    }
    let n = system.or(microphone).map_or(0, <[f32]>::len);
    if n == 0
        || !n.is_multiple_of(2)
        || n > MAX_AUDIO_FRAMES * 2
        || system.is_some_and(|s| s.len() != n)
        || microphone.is_some_and(|s| s.len() != n)
    {
        return Err(AppError::Capture("unaligned or oversized audio mix"));
    }
    let headroom = if o.mode == AudioMode::Both { 0.5 } else { 1.0 };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let a = system.map_or(0.0, |s| s[i]);
        let b = microphone.map_or(0.0, |s| s[i]);
        if !a.is_finite() || !b.is_finite() {
            return Err(AppError::Capture("nonfinite mixer input"));
        }
        let v = (a * f32::from(o.system_gain) / 100.0 + b * f32::from(o.microphone_gain) / 100.0)
            * headroom;
        out.push((v.clamp(-1.0, 1.0) * 32767.0).round() as i16);
    }
    Ok(out)
}
