//! Поток движка: события панели, сроки жестов и фоновые опросы фокуса и состояния агентов
//! (то, что в Python-версии делали таймеры Qt и пулы потоков в `gui.py`).

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::engine::Engine;
use crate::sessions::{Probe, Session, Sessions};

pub enum Event {
    /// Кнопка нажата (true) или отпущена.
    Raw(usize, bool),
    /// Удерживаемую кнопку начали перетаскивать.
    Drag(usize),
    /// Перетаскивание закончено над кнопкой (или мимо кнопок).
    Drop(usize, Option<usize>),
    /// Ответ в диалоге подтверждения.
    Answer(bool),
    /// Крестик на кнопке с закрытой сессией: сбросить кнопку.
    Forget(usize),
    /// Сделать профиль кнопок текущим (свайп или кнопка профиля на панели).
    Profile(usize),
    Focused(Option<Session>),
    /// Выделенная сессия, прочитанная в фоне для запоминания: номер чтения и результат.
    Current(u64, Result<Session, String>),
    Probed(Vec<(usize, Session, Probe)>),
    /// Запрос страницы управления; ответ — в канал.
    Page(PageCommand, Sender<Value>),
}

pub enum PageCommand {
    Overview,
    SetScenario(usize, Option<String>),
    Clear(usize),
    Setting(String, bool),
    /// Кнопка-сочетание: нажатие кнопки нажимает это сочетание; None — обычная кнопка.
    Keys(usize, Option<String>),
    /// Свой вид кнопки; пустой — автоматический.
    Look(usize, crate::look::Look),
    /// Сделать профиль кнопок текущим.
    Profile(usize),
    /// Настройка языка и сценарии, прочитанные на новом языке.
    Language(String, std::collections::BTreeMap<String, crate::scenarios::Scenario>),
}

pub struct Polls {
    /// Как часто искать сессию в фокусе для рамки; 0 — не искать.
    pub focus_ms: u64,
    /// Как часто проверять состояние агентов и доступность сессий; 0 — не проверять.
    pub status_ms: u64,
    /// Запустить WSL заранее (есть сервер herdr в WSL).
    pub warm_up_wsl: bool,
}

/// Фоновый исполнитель: берёт последнюю задачу, отвечает событием.
fn worker<T: Send + 'static>(events: Sender<Event>, job: impl Fn(T) -> Event + Send + 'static) -> Sender<T> {
    let (tx, rx) = mpsc::channel::<T>();
    std::thread::spawn(move || {
        for task in rx {
            if events.send(job(task)).is_err() {
                return;
            }
        }
    });
    tx
}

pub fn spawn(mut engine: Engine, events_tx: Sender<Event>, events: Receiver<Event>, sessions: Arc<dyn Sessions>, polls: Polls) {
    std::thread::spawn(move || {
        let focus = {
            let sessions = sessions.clone();
            worker(events_tx.clone(), move |bound: Vec<Session>| Event::Focused(sessions.focused(&bound)))
        };
        let probe = {
            let sessions = sessions.clone();
            worker(events_tx.clone(), move |bound: Vec<(usize, Session)>| {
                let list: Vec<Session> = bound.iter().map(|(_, s)| s.clone()).collect();
                let results = sessions.probe(&list);
                Event::Probed(bound.into_iter().zip(results).map(|((key, session), probe)| (key, session, probe)).collect())
            })
        };
        let read = {
            let sessions = sessions.clone();
            worker(events_tx.clone(), move |id: u64| Event::Current(id, sessions.current().map_err(|e| e.to_string())))
        };
        drop(events_tx);
        // Процессы PowerShell и WSL — заранее, чтобы первое нажатие кнопки не ждало их запуска.
        let wsl = polls.warm_up_wsl;
        std::thread::spawn(move || crate::workers::warm_up(wsl));

        let every = |ms: u64| (ms > 0).then(|| Duration::from_millis(ms));
        let (focus_every, status_every) = (every(polls.focus_ms), every(polls.status_ms));
        let start = Instant::now();
        let (mut next_focus, mut next_status) = (start, start);
        let (mut focus_busy, mut status_busy) = (false, false);
        loop {
            let now = Instant::now();
            // Срок опроса, пока прежний ещё идёт, пропускается — как у таймера Python-версии.
            for id in engine.take_reads() {
                let _ = read.send(id);
            }
            let requested = engine.take_focus_request();
            if let Some(period) = focus_every {
                if now >= next_focus || requested {
                    if !focus_busy {
                        let bound: Vec<Session> = engine.bound_keys().into_iter().map(|(_, s)| s).collect();
                        if bound.is_empty() {
                            engine.show_focused(None);
                        } else {
                            focus_busy = focus.send(bound).is_ok();
                        }
                    }
                    next_focus = now + period;
                }
            }
            if let Some(period) = status_every {
                if now >= next_status {
                    let bound = engine.bound_keys();
                    if !status_busy && !bound.is_empty() {
                        status_busy = probe.send(bound).is_ok();
                    }
                    next_status = now + period;
                }
            }
            let deadline = [
                engine.next_deadline(),
                focus_every.map(|_| next_focus),
                status_every.map(|_| next_status),
            ]
            .into_iter()
            .flatten()
            .min();
            let received = match deadline {
                Some(at) => events.recv_timeout(at.saturating_duration_since(Instant::now())),
                None => events.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            let now = Instant::now();
            match received {
                Ok(Event::Raw(key, pressed)) => engine.on_raw(key, pressed, now),
                Ok(Event::Drag(key)) => engine.start_drag(key, now),
                Ok(Event::Drop(key, target)) => engine.drop(key, target, now),
                Ok(Event::Answer(accepted)) => engine.answer(accepted),
                Ok(Event::Forget(key)) => engine.forget_closed(key),
                Ok(Event::Profile(profile)) => engine.switch_profile(profile),
                Ok(Event::Focused(focused)) => {
                    focus_busy = false;
                    engine.show_focused(focused);
                }
                Ok(Event::Current(id, result)) => engine.on_current(id, result),
                Ok(Event::Probed(probes)) => {
                    status_busy = false;
                    engine.show_probes(probes);
                }
                Ok(Event::Page(command, reply)) => {
                    let answer = match command {
                        PageCommand::Overview => engine.overview(),
                        PageCommand::SetScenario(key, scenario) => {
                            engine.assign(key, scenario.as_deref());
                            json!({"ok": true})
                        }
                        PageCommand::Clear(key) => {
                            engine.clear(key);
                            json!({"ok": true})
                        }
                        PageCommand::Setting(name, value) => json!({"ok": engine.set_setting(&name, value)}),
                        PageCommand::Profile(profile) => {
                            engine.switch_profile(profile);
                            json!({"ok": true})
                        }
                        PageCommand::Keys(key, combo) => match engine.set_keys(key, combo.as_deref()) {
                            Ok(()) => json!({"ok": true}),
                            Err(error) => json!({"ok": false, "error": error}),
                        },
                        PageCommand::Look(key, look) => {
                            engine.set_look(key, look);
                            json!({"ok": true})
                        }
                        PageCommand::Language(setting, scenarios) => {
                            engine.set_language(&setting, scenarios);
                            json!({"ok": true})
                        }
                    };
                    let _ = reply.send(answer);
                }
                Err(RecvTimeoutError::Timeout) => engine.tick(now),
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
}
