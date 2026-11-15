//! QR-код ссылки на панель для страницы управления: байтовый режим, уровень коррекции M,
//! версии 1–10 (до 213 байт — ссылке с адресом и ключом хватает с запасом).
//! Построение — по ISO/IEC 18004, как в библиотеке Nayuki (Project Nayuki, MIT).

/// (блоков, данных в блоке) для двух групп и кодовых слов коррекции на блок — уровень M.
const BLOCKS_M: [(usize, usize, usize, usize, usize); 10] = [
    (1, 16, 0, 0, 10),
    (1, 28, 0, 0, 16),
    (1, 44, 0, 0, 26),
    (2, 32, 0, 0, 18),
    (2, 43, 0, 0, 24),
    (4, 27, 0, 0, 16),
    (4, 31, 0, 0, 18),
    (2, 38, 2, 39, 22),
    (3, 36, 2, 37, 22),
    (4, 43, 1, 44, 26),
];

const ALIGNMENT: [&[usize]; 10] = [
    &[],
    &[6, 18],
    &[6, 22],
    &[6, 26],
    &[6, 30],
    &[6, 34],
    &[6, 22, 38],
    &[6, 24, 42],
    &[6, 26, 46],
    &[6, 28, 50],
];

pub struct Qr {
    pub size: usize,
    /// [строка][столбец], true — тёмный модуль.
    pub modules: Vec<Vec<bool>>,
    function: Vec<Vec<bool>>,
}

fn gf_mul(x: u8, y: u8) -> u8 {
    let mut z: u16 = 0;
    for i in (0..8).rev() {
        z = (z << 1) ^ ((z >> 7) * 0x11D);
        z ^= ((y as u16 >> i) & 1) * x as u16;
    }
    z as u8
}

fn rs_divisor(degree: usize) -> Vec<u8> {
    let mut result = vec![0u8; degree];
    result[degree - 1] = 1;
    let mut root = 1u8;
    for _ in 0..degree {
        for j in 0..degree {
            result[j] = gf_mul(result[j], root);
            if j + 1 < degree {
                result[j] ^= result[j + 1];
            }
        }
        root = gf_mul(root, 0x02);
    }
    result
}

fn rs_remainder(data: &[u8], divisor: &[u8]) -> Vec<u8> {
    let mut result = vec![0u8; divisor.len()];
    for &byte in data {
        let factor = byte ^ result.remove(0);
        result.push(0);
        for (x, &y) in result.iter_mut().zip(divisor) {
            *x ^= gf_mul(y, factor);
        }
    }
    result
}

fn data_capacity(version: usize) -> usize {
    let (b1, d1, b2, d2, _) = BLOCKS_M[version - 1];
    b1 * d1 + b2 * d2
}

/// Кодовые слова: данные с заполнением, разбитые на блоки, коррекция, перемежение.
fn codewords(data: &[u8], version: usize) -> Vec<u8> {
    let capacity = data_capacity(version);
    let mut bits: Vec<bool> = Vec::new();
    let push = |value: usize, count: usize, bits: &mut Vec<bool>| {
        for i in (0..count).rev() {
            bits.push((value >> i) & 1 == 1);
        }
    };
    push(0b0100, 4, &mut bits);
    push(data.len(), if version <= 9 { 8 } else { 16 }, &mut bits);
    for &byte in data {
        push(byte as usize, 8, &mut bits);
    }
    let terminator = (capacity * 8 - bits.len()).min(4);
    push(0, terminator, &mut bits);
    while bits.len() % 8 != 0 {
        bits.push(false);
    }
    let mut bytes: Vec<u8> = bits.chunks(8).map(|c| c.iter().fold(0u8, |b, &bit| (b << 1) | bit as u8)).collect();
    for pad in [0xEC, 0x11].iter().cycle() {
        if bytes.len() >= capacity {
            break;
        }
        bytes.push(*pad);
    }

    let (b1, d1, b2, d2, ec) = BLOCKS_M[version - 1];
    let divisor = rs_divisor(ec);
    let mut blocks: Vec<(&[u8], Vec<u8>)> = Vec::new();
    let mut offset = 0;
    for length in std::iter::repeat_n(d1, b1).chain(std::iter::repeat_n(d2, b2)) {
        let block = &bytes[offset..offset + length];
        blocks.push((block, rs_remainder(block, &divisor)));
        offset += length;
    }
    let mut result = Vec::new();
    for i in 0..d1.max(d2) {
        result.extend(blocks.iter().filter_map(|(data, _)| data.get(i)));
    }
    for i in 0..ec {
        result.extend(blocks.iter().map(|(_, ecc)| ecc[i]));
    }
    result
}

fn masked(mask: u8, x: usize, y: usize) -> bool {
    match mask {
        0 => (x + y) % 2 == 0,
        1 => y % 2 == 0,
        2 => x % 3 == 0,
        3 => (x + y) % 3 == 0,
        4 => (x / 3 + y / 2) % 2 == 0,
        5 => x * y % 2 + x * y % 3 == 0,
        6 => (x * y % 2 + x * y % 3) % 2 == 0,
        _ => ((x + y) % 2 + x * y % 3) % 2 == 0,
    }
}

impl Qr {
    fn new(version: usize) -> Self {
        let size = version * 4 + 17;
        let mut qr = Self { size, modules: vec![vec![false; size]; size], function: vec![vec![false; size]; size] };
        for i in 0..size {
            qr.set(6, i, i % 2 == 0);
            qr.set(i, 6, i % 2 == 0);
        }
        for (x, y) in [(3, 3), (size - 4, 3), (3, size - 4)] {
            qr.finder(x, y);
        }
        let positions = ALIGNMENT[version - 1];
        let last = positions.len().saturating_sub(1);
        for (i, &x) in positions.iter().enumerate() {
            for (j, &y) in positions.iter().enumerate() {
                if !((i == 0 && j == 0) || (i == 0 && j == last) || (i == last && j == 0)) {
                    qr.alignment(x, y);
                }
            }
        }
        qr.format_bits(0); // резерв места; настоящие биты — после выбора маски
        qr.version_bits(version);
        qr
    }

    /// x — столбец, y — строка.
    fn set(&mut self, x: usize, y: usize, dark: bool) {
        self.modules[y][x] = dark;
        self.function[y][x] = true;
    }

    fn finder(&mut self, cx: usize, cy: usize) {
        for dy in -4i32..=4 {
            for dx in -4i32..=4 {
                let (x, y) = (cx as i32 + dx, cy as i32 + dy);
                if x < 0 || y < 0 || x >= self.size as i32 || y >= self.size as i32 {
                    continue;
                }
                let distance = dx.abs().max(dy.abs());
                self.set(x as usize, y as usize, distance != 2 && distance != 4);
            }
        }
    }

    fn alignment(&mut self, cx: usize, cy: usize) {
        for dy in -2i32..=2 {
            for dx in -2i32..=2 {
                self.set((cx as i32 + dx) as usize, (cy as i32 + dy) as usize, dx.abs().max(dy.abs()) != 1);
            }
        }
    }

    fn format_bits(&mut self, mask: u8) {
        let data = mask as u32; // уровень M — биты 00
        let mut rem = data;
        for _ in 0..10 {
            rem = (rem << 1) ^ ((rem >> 9) * 0x537);
        }
        let bits = ((data << 10) | rem) ^ 0x5412;
        let bit = |i: u32| (bits >> i) & 1 == 1;
        let size = self.size;
        for i in 0..=5 {
            self.set(8, i, bit(i as u32));
        }
        self.set(8, 7, bit(6));
        self.set(8, 8, bit(7));
        self.set(7, 8, bit(8));
        for i in 9..15 {
            self.set(14 - i, 8, bit(i as u32));
        }
        for i in 0..8 {
            self.set(size - 1 - i, 8, bit(i as u32));
        }
        for i in 8..15 {
            self.set(8, size - 15 + i, bit(i as u32));
        }
        self.set(8, size - 8, true);
    }

    fn version_bits(&mut self, version: usize) {
        if version < 7 {
            return;
        }
        let mut rem = version as u32;
        for _ in 0..12 {
            rem = (rem << 1) ^ ((rem >> 11) * 0x1F25);
        }
        let bits = ((version as u32) << 12) | rem;
        for i in 0..18 {
            let dark = (bits >> i) & 1 == 1;
            let (a, b) = (self.size - 11 + i % 3, i / 3);
            self.set(a, b, dark);
            self.set(b, a, dark);
        }
    }

    fn place(&mut self, data: &[u8]) {
        let size = self.size;
        let mut i = 0;
        let mut right = size - 1;
        loop {
            if right == 6 {
                right = 5;
            }
            for vertical in 0..size {
                for j in 0..2 {
                    let x = right - j;
                    let upward = (right + 1) & 2 == 0;
                    let y = if upward { size - 1 - vertical } else { vertical };
                    if !self.function[y][x] && i < data.len() * 8 {
                        self.modules[y][x] = (data[i >> 3] >> (7 - (i & 7))) & 1 == 1;
                        i += 1;
                    }
                }
            }
            if right < 2 {
                break;
            }
            right -= 2;
        }
    }

    fn apply_mask(&mut self, mask: u8) {
        for y in 0..self.size {
            for x in 0..self.size {
                if !self.function[y][x] && masked(mask, x, y) {
                    self.modules[y][x] ^= true;
                }
            }
        }
    }

    /// Штраф за неудобные для сканера узоры (правила 1–4 стандарта).
    fn penalty(&self) -> u32 {
        let size = self.size;
        let get = |x: usize, y: usize| self.modules[y][x];
        let mut score = 0u32;
        let lines = |transpose: bool| -> Vec<Vec<bool>> {
            (0..size).map(|a| (0..size).map(|b| if transpose { get(a, b) } else { get(b, a) }).collect()).collect()
        };
        for line in lines(false).into_iter().chain(lines(true)) {
            let mut run = 1;
            for i in 1..=size {
                if i < size && line[i] == line[i - 1] {
                    run += 1;
                    continue;
                }
                if run >= 5 {
                    score += 3 + (run - 5) as u32;
                }
                run = 1;
            }
            let pattern = [true, false, true, true, true, false, true];
            for start in 0..size.saturating_sub(6) {
                if line[start..start + 7] != pattern {
                    continue;
                }
                let light = |from: i32, to: i32| (from..to).all(|k| k < 0 || k >= size as i32 || !line[k as usize]);
                let s = start as i32;
                if light(s - 4, s) || light(s + 7, s + 11) {
                    score += 40;
                }
            }
        }
        for y in 0..size - 1 {
            for x in 0..size - 1 {
                let c = get(x, y);
                if c == get(x + 1, y) && c == get(x, y + 1) && c == get(x + 1, y + 1) {
                    score += 3;
                }
            }
        }
        let dark = self.modules.iter().flatten().filter(|&&d| d).count() as i64;
        let total = (size * size) as i64;
        let k = ((dark * 20 - total * 10).abs() + total - 1) / total - 1;
        score + 10 * k.max(0) as u32
    }
}

/// QR-код с маской `mask`, или с лучшей по штрафу, если None. None — данные не помещаются.
pub fn encode_with(data: &[u8], mask: Option<u8>) -> Option<Qr> {
    let version = (1..=10).find(|&v| 4 + if v <= 9 { 8 } else { 16 } + data.len() * 8 <= data_capacity(v) * 8)?;
    let words = codewords(data, version);
    let mut base = Qr::new(version);
    base.place(&words);
    let build = |mask: u8| {
        let mut qr = Qr { size: base.size, modules: base.modules.clone(), function: base.function.clone() };
        qr.apply_mask(mask);
        qr.format_bits(mask);
        qr
    };
    match mask {
        Some(mask) => Some(build(mask)),
        None => (0..8).map(build).min_by_key(Qr::penalty),
    }
}

/// SVG-картинка QR-кода с полем в 4 модуля.
pub fn svg(text: &str) -> Option<String> {
    let qr = encode_with(text.as_bytes(), None)?;
    let full = qr.size + 8;
    let mut path = String::new();
    for (y, row) in qr.modules.iter().enumerate() {
        for (x, &dark) in row.iter().enumerate() {
            if dark {
                path.push_str(&format!("M{},{}h1v1h-1z", x + 4, y + 4));
            }
        }
    }
    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {full} {full}\" shape-rendering=\"crispEdges\">\
         <rect width=\"{full}\" height=\"{full}\" fill=\"#fff\"/><path d=\"{path}\" fill=\"#000\"/></svg>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reed_solomon_matches_standard_example() {
        // Пример из стандарта: «01234567» в версии 1-M.
        let data = [0x10, 0x20, 0x0C, 0x56, 0x61, 0x80, 0xEC, 0x11, 0xEC, 0x11, 0xEC, 0x11, 0xEC, 0x11, 0xEC, 0x11];
        let ecc = rs_remainder(&data, &rs_divisor(10));
        assert_eq!(ecc, [0xA5, 0x24, 0xD4, 0xC1, 0xED, 0x36, 0xC7, 0x87, 0x2C, 0x55]);
    }

    #[test]
    fn version_grows_with_length() {
        assert_eq!(encode_with(b"hello", Some(0)).unwrap().size, 21);
        let url = "http://192.168.100.200:3000/mobile?key=0123456789abcdef0123456789abcdef";
        assert_eq!(encode_with(url.as_bytes(), None).unwrap().size, 37); // версия 5
        assert!(encode_with(&[b'a'; 213], None).is_some());
        assert!(encode_with(&[b'a'; 214], None).is_none());
    }

    #[test]
    fn finder_and_timing_patterns() {
        let qr = encode_with(b"test", None).unwrap();
        let row: String = qr.modules[0][..8].iter().map(|&d| if d { '#' } else { '.' }).collect();
        assert_eq!(row, "#######.");
        let timing: String = qr.modules[6][8..13].iter().map(|&d| if d { '#' } else { '.' }).collect();
        assert_eq!(timing, "#.#.#");
        assert!(qr.modules[qr.size - 8][8]); // тёмный модуль
    }
}
