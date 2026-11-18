//! Global hotkeys via `handy-keys` (low-level hook on Windows, event tap on macOS).
//! Registered hotkeys are blocked from reaching the focused app.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use handy_keys::{Hotkey, HotkeyId, HotkeyManager, HotkeyState};

#[derive(Clone, Debug)]
pub struct HotkeyEvent {
    pub name: String,
    pub pressed: bool,
    /// When the hook saw the key, so tap/hold decisions and timings don't include queueing delay.
    pub at: Instant,
}

struct State {
    manager: HotkeyManager,
    names: HashMap<u32, String>,
    ids: Vec<HotkeyId>,
    held: HashSet<String>,
    events: Sender<HotkeyEvent>,
}

/// Owns the hotkey hook. Bindings can be replaced at runtime.
#[derive(Clone)]
pub struct Hotkeys {
    state: Arc<Mutex<State>>,
}

const POLL: Duration = Duration::from_millis(5);

impl Hotkeys {
    /// Starts the hook and forwards press/release events for registered bindings to `events`.
    pub fn start(events: Sender<HotkeyEvent>) -> Result<Self> {
        let manager =
            HotkeyManager::new_with_blocking().context("starting the global hotkey listener")?;
        let state = Arc::new(Mutex::new(State {
            manager,
            names: HashMap::new(),
            ids: Vec::new(),
            held: HashSet::new(),
            events,
        }));
        let thread_state = state.clone();
        std::thread::Builder::new()
            .name("duma-hotkeys".into())
            .spawn(move || {
                loop {
                    let delivered = {
                        let mut state = thread_state.lock().expect("hotkey state");
                        let Some(raw) = state.manager.try_recv() else {
                            drop(state);
                            std::thread::sleep(POLL);
                            continue;
                        };
                        let Some(name) = state.names.get(&raw.id.as_u32()).cloned() else {
                            continue;
                        };
                        let pressed = raw.state == HotkeyState::Pressed;
                        if pressed {
                            state.held.insert(name.clone());
                        } else {
                            state.held.remove(&name);
                        }
                        state
                            .events
                            .send(HotkeyEvent {
                                name,
                                pressed,
                                at: Instant::now(),
                            })
                            .is_ok()
                    };
                    if !delivered {
                        return;
                    }
                }
            })?;
        Ok(Self { state })
    }

    /// Makes the registered bindings equal `bindings` (name → "Ctrl+Alt+Space"). Unchanged bindings are
    /// left alone, so a key being held is not disturbed. A removed binding that is held gets a release
    /// event, so hold-to-talk can't get stuck. Returns the bindings that failed, with the reason.
    pub fn set_bindings(&self, bindings: &[(String, String)]) -> Vec<(String, String)> {
        let mut state = self.state.lock().expect("hotkey state");
        let wanted: HashMap<String, Hotkey> = bindings
            .iter()
            .filter(|(_, b)| !b.trim().is_empty())
            .filter_map(|(n, b)| b.parse::<Hotkey>().ok().map(|h| (n.clone(), h)))
            .collect();
        let mut failed: Vec<(String, String)> = bindings
            .iter()
            .filter(|(_, b)| !b.trim().is_empty() && b.parse::<Hotkey>().is_err())
            .map(|(n, b)| (n.clone(), format!("{b}: not a valid shortcut")))
            .collect();

        // Drop bindings that were removed or changed.
        let ids = std::mem::take(&mut state.ids);
        for id in ids {
            let name = state.names.get(&id.as_u32()).cloned().unwrap_or_default();
            let current = state.manager.get_hotkey(id);
            if wanted.get(&name).is_some_and(|h| Some(*h) == current) {
                state.ids.push(id);
                continue;
            }
            let _ = state.manager.unregister(id);
            state.names.remove(&id.as_u32());
            if state.held.remove(&name) {
                let _ = state.events.send(HotkeyEvent {
                    name,
                    pressed: false,
                    at: Instant::now(),
                });
            }
        }
        // Add new ones.
        for (name, hotkey) in wanted {
            if state.names.values().any(|n| *n == name) {
                continue;
            }
            match state.manager.register(hotkey).map_err(|e| anyhow!(e)) {
                Ok(id) => {
                    state.names.insert(id.as_u32(), name);
                    state.ids.push(id);
                }
                Err(error) => failed.push((name, error.to_string())),
            }
        }
        failed
    }
}
