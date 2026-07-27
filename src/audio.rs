//! Захват микрофона через cpal: даунмикс в моно, ресемпл в 16 кГц,
//! замер уровня для волны и отправка сэмплов рабочему потоку.

use crate::shared::{send_level, WorkerMsg};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, StreamConfig};
use crossbeam_channel::Sender;

const TARGET_RATE: f64 = 16000.0;

/// Список имён устройств ввода (для окна настроек).
pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            if let Ok(name) = d.name() {
                names.push(name);
            }
        }
    }
    names
}

fn pick_device(name: Option<&str>) -> Result<Device, String> {
    let host = cpal::default_host();
    if let Some(n) = name {
        if let Ok(mut devices) = host.input_devices() {
            if let Some(d) = devices.find(|d| d.name().map(|x| x == n).unwrap_or(false)) {
                return Ok(d);
            }
        }
    }
    host.default_input_device()
        .ok_or_else(|| "Не найдено ни одного микрофона".to_string())
}

/// Простой линейный ресемплер моно-потока в 16 кГц + конвертация в i16.
struct Resampler {
    step: f64, // входных сэмплов на один выходной
    pos: f64,
}

impl Resampler {
    fn new(in_rate: f64) -> Self {
        Resampler {
            step: in_rate / TARGET_RATE,
            pos: 0.0,
        }
    }

    fn process(&mut self, mono: &[f32]) -> Vec<f32> {
        let n = mono.len();
        if n == 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity((n as f64 / self.step) as usize + 2);
        let mut idx = self.pos;
        while (idx as usize) < n {
            let i0 = idx as usize;
            let frac = (idx - i0 as f64) as f32;
            let s0 = mono[i0];
            let s1 = if i0 + 1 < n { mono[i0 + 1] } else { s0 };
            out.push(s0 + (s1 - s0) * frac);
            idx += self.step;
        }
        self.pos = idx - n as f64;
        out
    }
}

/// Строит и запускает поток захвата. Возвращённый Stream нужно держать живым.
pub fn build_stream(
    device_name: Option<&str>,
    tx: Sender<WorkerMsg>,
) -> Result<cpal::Stream, String> {
    let device = pick_device(device_name)?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("Нет конфигурации ввода: {e}"))?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels as usize;
    let in_rate = config.sample_rate.0 as f64;

    let mut resampler = Resampler::new(in_rate);
    let err_fn = |e| eprintln!("[audio] ошибка потока: {e}");

    // адаптивный опорный уровень RMS (авто-усиление под конкретный микрофон)
    let mut ref_lvl = 0.02f32;

    // общий обработчик моно-блока
    let mut handle_mono = move |mono: Vec<f32>| {
        // уровень для волны: RMS с шумовым гейтом и нормировкой к недавнему пику
        if !mono.is_empty() {
            let sum: f32 = mono.iter().map(|x| x * x).sum();
            let rms = (sum / mono.len() as f32).sqrt();
            // опорный пик: мгновенная атака, медленный спад — подстраивается под голос
            if rms > ref_lvl {
                ref_lvl = rms;
            } else {
                ref_lvl += (rms - ref_lvl) * 0.003;
            }
            let refv = ref_lvl.max(0.012); // не делим на слишком малое (тихая комната)
            let gated = (rms - 0.0018).max(0.0); // отсечь фоновый шум
            let level = (gated / (refv * 0.6)).clamp(0.0, 1.0);
            send_level((level * 1000.0) as u32);
        }
        let samples = resampler.process(&mono);
        if !samples.is_empty() {
            let _ = tx.send(WorkerMsg::Audio(samples));
        }
    };

    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| handle_mono(downmix_f32(data, channels)),
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| {
                let f: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                handle_mono(downmix_f32(&f, channels))
            },
            err_fn,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| {
                let f: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0 - 1.0).collect();
                handle_mono(downmix_f32(&f, channels))
            },
            err_fn,
            None,
        ),
        other => return Err(format!("Неподдерживаемый формат сэмплов: {other:?}")),
    }
    .map_err(|e| format!("Не удалось открыть поток: {e}"))?;

    stream.play().map_err(|e| format!("play(): {e}"))?;
    Ok(stream)
}

fn downmix_f32(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    data.chunks(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect()
}
