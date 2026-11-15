//! Превращает сырые нажатия и отпускания в жесты (перенос `gestures.py`).
//!
//! Таймеров у распознавателя нет: он хранит сроки, а цикл движка вызывает `tick`,
//! когда наступает `next_deadline`. Время передаётся снаружи — так тесты не ждут.

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Gesture {
    Click,
    DoubleClick,
    HoldStart,
    HoldEnd,
    /// Удержание перешло в перетаскивание кнопки: вместо hold_end.
    HoldCancel,
    /// Нажатие и сразу нажатие с удержанием.
    DoubleHoldStart,
    DoubleHoldEnd,
}

impl Gesture {
    const ALL: [Gesture; 7] = [
        Gesture::Click,
        Gesture::DoubleClick,
        Gesture::HoldStart,
        Gesture::HoldEnd,
        Gesture::HoldCancel,
        Gesture::DoubleHoldStart,
        Gesture::DoubleHoldEnd,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Gesture::Click => "click",
            Gesture::DoubleClick => "double_click",
            Gesture::HoldStart => "hold_start",
            Gesture::HoldEnd => "hold_end",
            Gesture::HoldCancel => "hold_cancel",
            Gesture::DoubleHoldStart => "double_hold_start",
            Gesture::DoubleHoldEnd => "double_hold_end",
        }
    }

    pub fn parse(name: &str) -> Option<Gesture> {
        Self::ALL.into_iter().find(|g| g.name() == name)
    }
}

/// Что знает о кнопке сценарий: есть ли обработчик жеста и срабатывает ли клик сразу.
pub trait Handlers {
    fn handles(&self, key: usize, gesture: Gesture) -> bool;
    /// Клик — сразу при отпускании, а второе нажатие в срок даёт ещё и двойной клик.
    fn eager(&self, key: usize) -> bool;
}

#[derive(Default)]
struct KeyState {
    hold_at: Option<Instant>,
    /// Срок ожидания второго нажатия; true — по сроку будет клик (у «быстрого» клика — нет).
    click_at: Option<(Instant, bool)>,
    second_press: bool,
    /// Жест окончания текущего удержания.
    holding: Option<Gesture>,
    /// Нажатие ушло в перетаскивание: отпускание ничего не значит.
    cancelled: bool,
}

pub type Emitted = Vec<(usize, Gesture)>;

pub struct Recognizer {
    double_click: Duration,
    hold: Duration,
    keys: HashMap<usize, KeyState>,
}

impl Recognizer {
    pub fn new(double_click_ms: u64, hold_ms: u64) -> Self {
        Self {
            double_click: Duration::from_millis(double_click_ms),
            hold: Duration::from_millis(hold_ms),
            keys: HashMap::new(),
        }
    }

    pub fn on_raw(&mut self, key: usize, pressed: bool, now: Instant, handlers: &dyn Handlers) -> Emitted {
        let mut out = self.tick(now);
        let state = self.keys.entry(key).or_default();
        if pressed {
            if state.click_at.take().is_some() {
                state.second_press = true;
            }
            state.hold_at = Some(now + self.hold);
            return out;
        }
        if state.cancelled {
            state.cancelled = false;
            return out;
        }
        if let Some(gesture) = state.holding.take() {
            out.push((key, gesture));
            return out;
        }
        state.hold_at = None;
        if state.second_press {
            state.second_press = false;
            // Без обработчика двойного клика быстрое двойное нажатие — просто клик.
            let double = handlers.handles(key, Gesture::DoubleClick);
            out.push((key, if double { Gesture::DoubleClick } else { Gesture::Click }));
        } else if handlers.eager(key) {
            out.push((key, Gesture::Click));
            state.click_at = Some((now + self.double_click, false));
        } else if handlers.handles(key, Gesture::DoubleClick) || handlers.handles(key, Gesture::DoubleHoldStart) {
            state.click_at = Some((now + self.double_click, true));
        } else {
            out.push((key, Gesture::Click));
        }
        out
    }

    /// Нажатие стало перетаскиванием: начатое удержание отменяется (hold_cancel),
    /// а отпускание кнопки жестов не даёт.
    pub fn cancel(&mut self, key: usize) -> Emitted {
        let state = self.keys.entry(key).or_default();
        state.hold_at = None;
        state.second_press = false;
        state.cancelled = true;
        match state.holding.take() {
            Some(Gesture::HoldEnd) => vec![(key, Gesture::HoldCancel)],
            // У двойного удержания отмены нет: просто окончание.
            Some(gesture) => vec![(key, gesture)],
            None => vec![],
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.keys.values().flat_map(|s| [s.hold_at, s.click_at.map(|c| c.0)]).flatten().min()
    }

    /// Сработавшие к `now` сроки — по порядку наступления.
    pub fn tick(&mut self, now: Instant) -> Emitted {
        let mut out = Vec::new();
        while let Some((_, key, is_hold)) = self
            .keys
            .iter()
            .flat_map(|(&key, s)| [s.hold_at.map(|t| (t, key, true)), s.click_at.map(|c| (c.0, key, false))])
            .flatten()
            .filter(|(at, _, _)| *at <= now)
            .min_by_key(|(at, key, _)| (*at, *key))
        {
            let state = self.keys.get_mut(&key).unwrap();
            if is_hold {
                state.hold_at = None;
                if state.second_press {
                    // Ни клика, ни обычного удержания.
                    state.second_press = false;
                    state.holding = Some(Gesture::DoubleHoldEnd);
                    out.push((key, Gesture::DoubleHoldStart));
                } else {
                    state.holding = Some(Gesture::HoldEnd);
                    out.push((key, Gesture::HoldStart));
                }
            } else if let Some((_, emit)) = state.click_at.take() {
                if emit {
                    out.push((key, Gesture::Click));
                }
            }
        }
        out
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use Gesture::*;

    pub struct Fake {
        pub handled: Vec<Gesture>,
        pub eager: bool,
    }

    impl Handlers for Fake {
        fn handles(&self, _key: usize, gesture: Gesture) -> bool {
            self.handled.contains(&gesture)
        }
        fn eager(&self, _key: usize) -> bool {
            self.eager
        }
    }

    /// Распознаватель с поддельным временем: `at` — миллисекунды от начала.
    struct Run {
        gestures: Recognizer,
        handlers: Fake,
        start: Instant,
        now: u64,
        events: Vec<Gesture>,
    }

    impl Run {
        fn new(handled: &[Gesture]) -> Self {
            Self::with(handled, false, 500)
        }
        fn with(handled: &[Gesture], eager: bool, hold_ms: u64) -> Self {
            Self {
                gestures: Recognizer::new(300, hold_ms),
                handlers: Fake { handled: handled.to_vec(), eager },
                start: Instant::now(),
                now: 0,
                events: vec![],
            }
        }
        fn instant(&self) -> Instant {
            self.start + Duration::from_millis(self.now)
        }
        fn raw(&mut self, pressed: bool) {
            let now = self.instant();
            let out = self.gestures.on_raw(0, pressed, now, &self.handlers);
            self.events.extend(out.into_iter().map(|(_, g)| g));
        }
        fn advance(&mut self, ms: u64) {
            self.now += ms;
            let now = self.instant();
            let out = self.gestures.tick(now);
            self.events.extend(out.into_iter().map(|(_, g)| g));
        }
        fn tap(&mut self) {
            self.raw(true);
            self.advance(50);
            self.raw(false);
        }
    }

    const BASIC: &[Gesture] = &[Click, DoubleClick, HoldStart, HoldEnd];

    #[test]
    fn click_fires_after_double_click_window() {
        let mut run = Run::new(BASIC);
        run.tap();
        assert!(run.events.is_empty());
        run.advance(300);
        assert_eq!(run.events, [Click]);
    }

    #[test]
    fn click_is_immediate_without_double_click_handler() {
        let mut run = Run::new(&[Click]);
        run.tap();
        assert_eq!(run.events, [Click]);
    }

    #[test]
    fn double_click() {
        let mut run = Run::new(BASIC);
        run.tap();
        run.advance(100);
        run.tap();
        run.advance(1000);
        assert_eq!(run.events, [DoubleClick]);
    }

    #[test]
    fn hold() {
        let mut run = Run::new(BASIC);
        run.raw(true);
        run.advance(500);
        assert_eq!(run.events, [HoldStart]);
        run.advance(2000);
        run.raw(false);
        run.advance(1000);
        assert_eq!(run.events, [HoldStart, HoldEnd]);
    }

    #[test]
    fn double_hold_is_neither_click_nor_hold() {
        for handled in [&[Click, DoubleClick, HoldStart][..], &[Click, DoubleHoldStart]] {
            let mut run = Run::new(handled);
            run.tap();
            run.advance(100);
            run.raw(true);
            run.advance(500);
            assert_eq!(run.events, [DoubleHoldStart]);
            run.advance(2000);
            run.raw(false);
            run.advance(1000);
            assert_eq!(run.events, [DoubleHoldStart, DoubleHoldEnd]);
        }
    }

    #[test]
    fn click_waits_for_second_press_when_only_double_hold_is_handled() {
        let mut run = Run::new(&[Click, DoubleHoldStart]);
        run.tap();
        assert!(run.events.is_empty());
        run.advance(300);
        assert_eq!(run.events, [Click]);
    }

    #[test]
    fn quick_double_press_without_double_click_handler_is_one_click() {
        let mut run = Run::new(&[Click, DoubleHoldStart]);
        run.tap();
        run.advance(100);
        run.tap();
        run.advance(1000);
        assert_eq!(run.events, [Click]);
    }

    #[test]
    fn hold_threshold_is_configurable() {
        let mut run = Run::with(&[Click, DoubleClick, HoldStart, HoldEnd, HoldCancel, DoubleHoldStart], false, 800);
        run.raw(true);
        run.advance(500);
        assert!(run.events.is_empty());
        run.advance(300);
        assert_eq!(run.events, [HoldStart]);
    }

    #[test]
    fn eager_click_fires_on_release_and_second_press_adds_double_click() {
        let mut run = Run::with(&[Click, DoubleClick], true, 500);
        run.tap();
        assert_eq!(run.events, [Click]);
        run.advance(50);
        run.tap();
        assert_eq!(run.events, [Click, DoubleClick]);
        run.advance(1000);
        run.tap();
        assert_eq!(run.events.last(), Some(&Click));
    }

    #[test]
    fn cancelled_hold_gives_hold_cancel_and_release_is_silent() {
        let mut run = Run::new(&[Click, HoldStart, HoldEnd, HoldCancel]);
        run.raw(true);
        run.advance(500);
        let out = run.gestures.cancel(0);
        run.events.extend(out.into_iter().map(|(_, g)| g));
        run.raw(false);
        run.advance(1000);
        assert_eq!(run.events, [HoldStart, HoldCancel]);
        run.tap();
        assert_eq!(run.events.last(), Some(&Click));
    }

    #[test]
    fn release_after_missed_tick_still_gives_hold() {
        // Цикл движка мог не успеть к сроку удержания: отпускание сначала досчитывает сроки.
        let mut run = Run::new(BASIC);
        run.raw(true);
        run.now += 700;
        run.raw(false);
        assert_eq!(run.events, [HoldStart, HoldEnd]);
    }
}
