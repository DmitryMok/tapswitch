//! Живая иконка окна на кнопке: если программа показывает в окне иконку, отличную от иконки
//! своего exe (счётчик, состояние, график), кнопка показывает её и обновляется вместе с ней.
//!
//! Проверка окон (`windows.rs`, раз в несколько секунд) кладёт сюда PNG и записывает в сессию
//! ключ `live_icon`; кнопка берёт картинку по адресу `/winicon/<ключ>.png`. В ключе — номер
//! процесса Tapswitch, поэтому ключ, сохранённый в state.json прошлым запуском, не найдётся,
//! и первая же проверка после перезапуска перерисует кнопку.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

/// Иконка программы на кнопке, `/icon_url` → значение шаблона.
pub const PREFIX: &str = "winicon:";
/// Поле сессии окна с ключом живой иконки; пусто — иконка как у exe.
pub const FIELD: &str = "live_icon";
/// Размер PNG для кнопок — как у иконок программ.
pub const SIZE: u32 = 64;
/// Размер, в котором иконка окна сравнивается с иконкой exe.
const COMPARE: u32 = 32;
/// Средняя разница каналов (0–255), ниже которой иконки считаются одинаковыми.
const SAME: f64 = 10.0;
/// Сколько последних картинок хранить: кнопок немного, старые ключи никому не нужны.
const KEEP: usize = 64;

static STORE: Mutex<VecDeque<(String, Vec<u8>)>> = Mutex::new(VecDeque::new());
static EXE_ICONS: OnceLock<Mutex<HashMap<String, Option<Vec<u8>>>>> = OnceLock::new();

pub fn png(key: &str) -> Option<Vec<u8>> {
    STORE.lock().unwrap().iter().find(|(k, _)| k == key).map(|(_, png)| png.clone())
}

pub fn exists(key: &str) -> bool {
    STORE.lock().unwrap().iter().any(|(k, _)| k == key)
}

pub(crate) fn remember(key: &str, png: Vec<u8>) {
    let mut store = STORE.lock().unwrap();
    if store.iter().any(|(k, _)| k == key) {
        return;
    }
    store.push_back((key.to_string(), png));
    while store.len() > KEEP {
        store.pop_front();
    }
}

/// Ключ живой иконки окна или пусто, если она такая же, как у exe (или её не прочитать).
pub fn of_window(window: i64, exe: &str) -> String {
    let Some((width, height, rgba)) = crate::win::window_icon(window) else { return String::new() };
    let small = scale(&rgba, width, height, COMPARE);
    let exe_icon = {
        let cache = EXE_ICONS.get_or_init(Default::default);
        let mut cache = cache.lock().unwrap();
        cache.entry(exe.to_lowercase()).or_insert_with(|| crate::win::program_icon(exe, COMPARE)).clone()
    };
    if exe_icon.is_some_and(|exe_icon| difference(&small, &exe_icon) < SAME) {
        return String::new();
    }
    let big = scale(&rgba, width, height, SIZE);
    let key = format!("{:x}-{:016x}", std::process::id(), fnv(&big));
    if !exists(&key) {
        remember(&key, crate::png::encode(SIZE, SIZE, &big));
    }
    key
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3))
}

/// Средняя разница цветов с учётом прозрачности (цвет прозрачной точки не важен).
fn difference(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return f64::MAX;
    }
    let total: f64 = a
        .chunks(4)
        .zip(b.chunks(4))
        .map(|(p, q)| {
            let (pa, qa) = (p[3] as f64 / 255.0, q[3] as f64 / 255.0);
            let channel = |i: usize| (p[i] as f64 * pa - q[i] as f64 * qa).abs();
            channel(0) + channel(1) + channel(2) + (p[3] as f64 - q[3] as f64).abs()
        })
        .sum();
    total / a.len() as f64
}

/// Квадрат `size`×`size` из RGBA `width`×`height`: среднее по области (уменьшение) или
/// билинейно (увеличение), с premultiplied alpha — чтобы края не темнели.
pub fn scale(rgba: &[u8], width: u32, height: u32, size: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let at = |x: usize, y: usize| -> [f64; 4] {
        let i = (y.min(h - 1) * w + x.min(w - 1)) * 4;
        let a = rgba[i + 3] as f64 / 255.0;
        [rgba[i] as f64 * a, rgba[i + 1] as f64 * a, rgba[i + 2] as f64 * a, a]
    };
    let mut out = vec![0u8; (size * size * 4) as usize];
    let (sx, sy) = (w as f64 / size as f64, h as f64 / size as f64);
    for ty in 0..size as usize {
        for tx in 0..size as usize {
            let mut sum = [0.0f64; 4];
            if sx >= 1.0 && sy >= 1.0 {
                // Уменьшение: среднее по точкам исходника, попавшим в целевую.
                let (x0, x1) = ((tx as f64 * sx) as usize, (((tx + 1) as f64 * sx).ceil() as usize).max(1));
                let (y0, y1) = ((ty as f64 * sy) as usize, (((ty + 1) as f64 * sy).ceil() as usize).max(1));
                let mut n = 0.0f64;
                for y in y0..y1.min(h) {
                    for x in x0..x1.min(w) {
                        let p = at(x, y);
                        for c in 0..4 {
                            sum[c] += p[c];
                        }
                        n += 1.0;
                    }
                }
                for c in &mut sum {
                    *c /= n.max(1.0);
                }
            } else {
                let fx = ((tx as f64 + 0.5) * sx - 0.5).max(0.0);
                let fy = ((ty as f64 + 0.5) * sy - 0.5).max(0.0);
                let (x0, y0) = (fx as usize, fy as usize);
                let (dx, dy) = (fx - x0 as f64, fy - y0 as f64);
                let corners = [(at(x0, y0), (1.0 - dx) * (1.0 - dy)), (at(x0 + 1, y0), dx * (1.0 - dy)),
                               (at(x0, y0 + 1), (1.0 - dx) * dy), (at(x0 + 1, y0 + 1), dx * dy)];
                for (p, weight) in corners {
                    for c in 0..4 {
                        sum[c] += p[c] * weight;
                    }
                }
            }
            let i = (ty * size as usize + tx) * 4;
            let a = sum[3];
            for c in 0..3 {
                out[i + c] = if a > 0.0 { (sum[c] / a).round().clamp(0.0, 255.0) as u8 } else { 0 };
            }
            out[i + 3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(size: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter().copied().cycle().take((size * size * 4) as usize).collect()
    }

    #[test]
    fn scale_keeps_solid_color_both_ways() {
        let red = solid(128, [200, 10, 20, 255]);
        assert_eq!(scale(&red, 128, 128, 64), solid(64, [200, 10, 20, 255]));
        let small = solid(16, [200, 10, 20, 255]);
        assert_eq!(scale(&small, 16, 16, 64), solid(64, [200, 10, 20, 255]));
    }

    #[test]
    fn scale_averages_and_ignores_color_of_transparent_pixels() {
        // 2×2: две непрозрачные белые точки и две прозрачные «чёрные» → одна точка: белая, полупрозрачная.
        let src = [255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255];
        assert_eq!(scale(&src, 2, 2, 1), vec![255, 255, 255, 128]);
    }

    #[test]
    fn difference_tells_same_icon_from_another() {
        let a = solid(32, [76, 175, 80, 255]);
        let mut b = a.clone();
        b[0] = 80; // едва заметная разница — та же иконка
        assert!(difference(&a, &b) < SAME);
        assert!(difference(&a, &solid(32, [239, 83, 80, 255])) > SAME);
        // Цвет полностью прозрачных точек не важен.
        assert_eq!(difference(&solid(4, [1, 2, 3, 0]), &solid(4, [9, 9, 9, 0])), 0.0);
    }

    #[test]
    fn store_keeps_recent_pictures() {
        remember("test-1", vec![1]);
        assert!(exists("test-1"));
        assert_eq!(png("test-1"), Some(vec![1]));
        for i in 0..KEEP {
            remember(&format!("test-fill-{i}"), vec![]);
        }
        assert!(!exists("test-1"));
    }
}
