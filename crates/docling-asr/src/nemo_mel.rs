//! NeMo's `AudioToMelSpectrogramPreprocessor` front-end for the Parakeet
//! models (#508) — a port of onnx-asr's `NemoPreprocessorNumpy`, the CPU
//! default of the reference runtime for the ONNX exports
//! (`istupakov/parakeet-tdt-0.6b-v3-onnx`), which reproduces NeMo's
//! `FilterbankFeatures` with the model's config:
//!
//! * pre-emphasis 0.97 (`y[i] = x[i] − 0.97·x[i−1]`, `y[0] = x[0]`);
//! * STFT: `n_fft` 512, hop 160 (10 ms), a 400-sample symmetric Hann window
//!   (`np.hanning(400)`, i.e. `torch.hann_window(400, periodic=False)`)
//!   centred in the 512-point frame, the signal zero-padded `n_fft / 2` on
//!   both sides (NeMo's `pad_mode="constant"`), `len / 160 + 1` frames;
//! * power spectrum (`|rfft|²`), 128 Slaney-normalized mel filters over
//!   0–8000 Hz (librosa `filters.mel(sr=16000, n_fft=512, n_mels=128)`);
//! * `ln(mel + 2⁻²⁴)`;
//! * per-feature normalization over the `len / 160` valid frames — mean and
//!   the unbiased standard deviation (`n − 1`), `(x − mean) / (std + 1e-5)`
//!   — with the frames past the valid length zeroed.
//!
//! The arithmetic follows numpy's dtypes (frames × window and the FFT in
//! `f64`, the magnitude cast to `f32` before squaring, the mel matmul and
//! everything after it in `f32`) so the features match the reference to
//! float rounding. The FFT is a radix-2 Cooley–Tukey over 512 points — the
//! Whisper front-end's naive DFT would be 2.5× the work per frame at this
//! size and the Parakeet path is meant for long recordings.

/// FFT size.
pub const N_FFT: usize = 512;
/// Window length (25 ms).
pub const WIN_LENGTH: usize = 400;
/// Hop length (10 ms).
pub const HOP_LENGTH: usize = 160;
/// Mel bands (Parakeet v3's `features_size`).
pub const N_MELS: usize = 128;
const N_BINS: usize = N_FFT / 2 + 1;
const PREEMPH: f32 = 0.97;
/// NeMo's `log_zero_guard_value` (2⁻²⁴).
const LOG_GUARD: f32 = 5.960_464_5e-8;

/// The features of `samples` (16 kHz mono): `(features, frames, valid)` with
/// `features` laid out `N_MELS × frames` row-major (the encoder's
/// `audio_signal` `[1, 128, frames]`), and `valid = len / 160` the frames the
/// normalization covers — the encoder's `length` input. Frames from `valid`
/// on are zero.
pub fn features(samples: &[f32]) -> (Vec<f32>, usize, usize) {
    let n = samples.len();
    let valid = n / HOP_LENGTH;
    let frames = n / HOP_LENGTH + 1;
    let half = N_FFT / 2;

    // Pre-emphasis, then constant (zero) padding by n_fft/2 on both sides.
    let mut padded = vec![0f32; n + 2 * half];
    for i in 0..n {
        let prev = if i == 0 { 0.0 } else { samples[i - 1] };
        padded[half + i] = samples[i] - PREEMPH * prev;
    }

    let window = window();
    let filters = mel_filters();
    let fft = Fft512::new();

    let mut log_mel = vec![0f32; N_MELS * frames];
    let mut re = [0f64; N_FFT];
    let mut im = [0f64; N_FFT];
    let mut power = [0f32; N_BINS];
    for t in 0..frames {
        let start = t * HOP_LENGTH;
        for i in 0..N_FFT {
            re[i] = padded[start + i] as f64 * window[i];
            im[i] = 0.0;
        }
        fft.run(&mut re, &mut im);
        for k in 0..N_BINS {
            let mag = (re[k] * re[k] + im[k] * im[k]).sqrt() as f32;
            power[k] = mag * mag;
        }
        for m in 0..N_MELS {
            let mut acc = 0f32;
            for k in 0..N_BINS {
                acc += power[k] * filters[k * N_MELS + m];
            }
            log_mel[m * frames + t] = (acc + LOG_GUARD).ln();
        }
    }

    // Per-feature normalization over the valid frames; the rest zeroed.
    for m in 0..N_MELS {
        let row = &mut log_mel[m * frames..(m + 1) * frames];
        if valid == 0 {
            row.iter_mut().for_each(|v| *v = 0.0);
            continue;
        }
        let sum: f32 = row[..valid].iter().sum();
        let mean = sum / valid as f32;
        let sq: f32 = row[..valid].iter().map(|v| (v - mean) * (v - mean)).sum();
        // numpy divides by `valid - 1` (inf/nan for a single frame — the
        // features of a 10–20 ms clip are meaningless either way).
        let var = sq / (valid as f32 - 1.0);
        let denom = var.sqrt() + 1e-5;
        for (t, v) in row.iter_mut().enumerate() {
            *v = if t < valid { (*v - mean) / denom } else { 0.0 };
        }
    }
    (log_mel, frames, valid)
}

/// `np.hanning(400)` (symmetric), zero-padded to 512, centred.
fn window() -> Vec<f64> {
    let off = (N_FFT - WIN_LENGTH) / 2;
    let mut w = vec![0f64; N_FFT];
    for i in 0..WIN_LENGTH {
        let x = 2.0 * std::f64::consts::PI * i as f64 / (WIN_LENGTH - 1) as f64;
        w[off + i] = 0.5 - 0.5 * x.cos();
    }
    w
}

/// The Slaney-normalized mel filterbank, laid out `N_BINS × N_MELS` (the
/// `fbanks.npz["nemo128"]` matrix onnx-asr multiplies the spectrum with),
/// computed the way librosa does in `f64` and cast to `f32`.
fn mel_filters() -> Vec<f32> {
    const SR: f64 = 16_000.0;
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = (6.4f64).ln() / 27.0;
    let hz_to_mel = |hz: f64| {
        if hz >= min_log_hz {
            min_log_mel + (hz / min_log_hz).ln() / logstep
        } else {
            hz / f_sp
        }
    };
    let mel_to_hz = |mel: f64| {
        if mel >= min_log_mel {
            min_log_hz * (logstep * (mel - min_log_mel)).exp()
        } else {
            f_sp * mel
        }
    };
    let max_mel = hz_to_mel(SR / 2.0);
    let mel_f: Vec<f64> = (0..N_MELS + 2)
        .map(|i| mel_to_hz(max_mel * i as f64 / (N_MELS + 1) as f64))
        .collect();
    let mut filters = vec![0f32; N_BINS * N_MELS];
    for m in 0..N_MELS {
        let (f0, f1, f2) = (mel_f[m], mel_f[m + 1], mel_f[m + 2]);
        let enorm = 2.0 / (f2 - f0);
        for k in 0..N_BINS {
            let freq = k as f64 * SR / N_FFT as f64;
            let lower = (freq - f0) / (f1 - f0);
            let upper = (f2 - freq) / (f2 - f1);
            filters[k * N_MELS + m] = (lower.min(upper).max(0.0) * enorm) as f32;
        }
    }
    filters
}

/// In-place iterative radix-2 FFT over 512 points (`f64`), forward
/// transform with numpy's sign convention.
struct Fft512 {
    cos: Vec<f64>,
    sin: Vec<f64>,
    rev: Vec<usize>,
}

impl Fft512 {
    fn new() -> Self {
        let bits = N_FFT.trailing_zeros();
        let rev = (0..N_FFT)
            .map(|i| i.reverse_bits() >> (usize::BITS - bits))
            .collect();
        let (cos, sin) = (0..N_FFT / 2)
            .map(|k| {
                let a = -2.0 * std::f64::consts::PI * k as f64 / N_FFT as f64;
                (a.cos(), a.sin())
            })
            .unzip();
        Self { cos, sin, rev }
    }

    fn run(&self, re: &mut [f64; N_FFT], im: &mut [f64; N_FFT]) {
        for i in 0..N_FFT {
            let j = self.rev[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= N_FFT {
            let step = N_FFT / len;
            for start in (0..N_FFT).step_by(len) {
                for k in 0..len / 2 {
                    let (wr, wi) = (self.cos[k * step], self.sin[k * step]);
                    let (a, b) = (start + k, start + k + len / 2);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values read out of onnx-asr's `fbanks.npz["nemo128"]` (librosa's
    /// filterbank as the reference runtime ships it).
    #[test]
    fn filterbank_matches_the_reference_matrix() {
        let fb = mel_filters();
        let sum: f64 = fb.iter().map(|&v| v as f64).sum();
        assert!((sum - 4.090_487_5).abs() < 1e-4, "sum {sum}");
        let row10_max = (0..N_MELS)
            .map(|m| fb[10 * N_MELS + m])
            .fold(0f32, f32::max);
        assert!((row10_max - 0.027_176_114).abs() < 1e-7, "{row10_max}");
    }

    #[test]
    fn fft_matches_a_direct_dft() {
        let fft = Fft512::new();
        let mut re = [0f64; N_FFT];
        let mut im = [0f64; N_FFT];
        let input: Vec<f64> = (0..N_FFT)
            .map(|i| ((i * 7919) % 1000) as f64 / 500.0 - 1.0)
            .collect();
        re.copy_from_slice(&input);
        fft.run(&mut re, &mut im);
        for k in [0usize, 1, 17, 128, 255, 256] {
            let (mut r, mut i) = (0f64, 0f64);
            for (n, x) in input.iter().enumerate() {
                let a = -2.0 * std::f64::consts::PI * (k * n) as f64 / N_FFT as f64;
                r += x * a.cos();
                i += x * a.sin();
            }
            assert!(
                (re[k] - r).abs() < 1e-9 && (im[k] - i).abs() < 1e-9,
                "bin {k}"
            );
        }
    }

    /// Frame count and normalization: `len/160 + 1` frames, zero mean and unit
    /// variance per band over the valid ones, zeros after.
    #[test]
    fn features_are_normalized_per_band() {
        let samples: Vec<f32> = (0..16_000)
            .map(|i| (i as f32 * 0.05).sin() * 0.3 + ((i * 31) % 97) as f32 / 970.0)
            .collect();
        let (f, frames, valid) = features(&samples);
        assert_eq!((frames, valid), (101, 100));
        for m in [0usize, 40, 127] {
            let row = &f[m * frames..(m + 1) * frames];
            let mean: f32 = row[..valid].iter().sum::<f32>() / valid as f32;
            let var: f32 =
                row[..valid].iter().map(|v| (v - mean).powi(2)).sum::<f32>() / (valid - 1) as f32;
            assert!(mean.abs() < 1e-4, "band {m} mean {mean}");
            assert!((var - 1.0).abs() < 1e-2, "band {m} var {var}");
            assert_eq!(row[valid], 0.0);
        }
    }
}
