use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Shortcut {
    pub command: bool,
    pub option: bool,
    pub control: bool,
    pub shift: bool,
    pub key: String,
}
impl Default for Shortcut {
    fn default() -> Self {
        Self {
            command: false,
            option: true,
            control: false,
            shift: false,
            key: "Space".into(),
        }
    }
}
impl Shortcut {
    pub fn label(&self) -> String {
        format!(
            "{}{}{}{} {}",
            if self.control { "⌃" } else { "" },
            if self.option { "⌥" } else { "" },
            if self.shift { "⇧" } else { "" },
            if self.command { "⌘" } else { "" },
            self.key
        )
    }
    pub fn keys() -> Vec<String> {
        std::iter::once("Space".into())
            .chain(('A'..='Z').map(|c| c.to_string()))
            .chain((0..=9).map(|n| n.to_string()))
            .chain((1..=12).map(|n| format!("F{n}")))
            .collect()
    }
    pub fn parse(&self) -> Result<HotKey, String> {
        if !(self.command || self.option || self.control) {
            return Err("至少选择 ⌘、⌥、⌃ 中的一个，避免影响普通输入".into());
        }
        if !Self::keys().contains(&self.key) {
            return Err("请选择空格、字母、数字或 F1–F12".into());
        }
        let mut parts = Vec::new();
        if self.command {
            parts.push("Super");
        }
        if self.option {
            parts.push("Alt");
        }
        if self.control {
            parts.push("Control");
        }
        if self.shift {
            parts.push("Shift");
        }
        parts.push(&self.key);
        parts
            .join("+")
            .parse::<HotKey>()
            .map_err(|e| format!("快捷键无效：{e}"))
    }
}
trait Registrar {
    fn register_key(&self, key: HotKey) -> Result<(), String>;
    fn unregister_key(&self, key: HotKey) -> Result<(), String>;
}
impl Registrar for GlobalHotKeyManager {
    fn register_key(&self, key: HotKey) -> Result<(), String> {
        self.register(key).map_err(|e| e.to_string())
    }
    fn unregister_key(&self, key: HotKey) -> Result<(), String> {
        self.unregister(key).map_err(|e| e.to_string())
    }
}
fn replace(registrar: &impl Registrar, old: Option<HotKey>, new: HotKey) -> Result<(), String> {
    if old == Some(new) {
        return Ok(());
    }
    registrar
        .register_key(new)
        .map_err(|e| format!("无法注册，可能被其他应用或系统占用：{e}；请换一个组合"))?;
    if let Some(old) = old {
        if let Err(error) = registrar.unregister_key(old) {
            let rollback = registrar.unregister_key(new);
            return Err(format!(
                "旧快捷键释放失败：{error}；新快捷键未保存{}",
                if rollback.is_err() {
                    "，请重启应用后重试"
                } else {
                    ""
                }
            ));
        }
    }
    Ok(())
}
pub struct Controller {
    manager: Option<GlobalHotKeyManager>,
    active: Option<HotKey>,
    active_id: Arc<AtomicU32>,
}
impl Controller {
    pub fn new(
        context: &egui::Context,
        requested: Arc<AtomicBool>,
        config: &Shortcut,
    ) -> (Self, Option<String>) {
        let active_id = Arc::new(AtomicU32::new(0));
        let id = active_id.clone();
        let repaint = context.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.id == id.load(Ordering::Acquire) && event.state == HotKeyState::Pressed {
                requested.store(true, Ordering::Release);
                repaint.request_repaint();
            }
        }));
        let manager = GlobalHotKeyManager::new();
        let error = manager
            .as_ref()
            .err()
            .map(|e| format!("快捷键初始化失败：{e}"));
        let mut controller = Self {
            manager: manager.ok(),
            active: None,
            active_id,
        };
        let error = error.or_else(|| controller.apply(config).err());
        (controller, error)
    }
    pub fn apply(&mut self, config: &Shortcut) -> Result<(), String> {
        let key = config.parse()?;
        if self.manager.is_none() {
            self.manager =
                Some(GlobalHotKeyManager::new().map_err(|e| format!("快捷键初始化失败：{e}"))?);
        }
        replace(self.manager.as_ref().unwrap(), self.active, key)?;
        self.active_id.store(key.id(), Ordering::Release);
        self.active = Some(key);
        Ok(())
    }
    pub fn active(&self) -> bool {
        self.active.is_some()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    struct Mock {
        keys: RefCell<Vec<HotKey>>,
        conflict: Option<HotKey>,
        fail_release: Option<HotKey>,
    }
    impl Registrar for Mock {
        fn register_key(&self, k: HotKey) -> Result<(), String> {
            if self.conflict == Some(k) {
                return Err("occupied".into());
            }
            self.keys.borrow_mut().push(k);
            Ok(())
        }
        fn unregister_key(&self, k: HotKey) -> Result<(), String> {
            if self.fail_release == Some(k) {
                return Err("release failed".into());
            }
            self.keys.borrow_mut().retain(|x| *x != k);
            Ok(())
        }
    }
    #[test]
    fn defaults_and_allowed_combinations() {
        let mut c = Shortcut::default();
        assert_eq!(c.label(), "⌥ Space");
        assert!(c.parse().is_ok());
        for key in Shortcut::keys() {
            c.key = key;
            assert!(c.parse().is_ok());
        }
        c.option = false;
        c.shift = true;
        assert!(c.parse().is_err());
        c.command = true;
        assert!(c.parse().is_ok());
        c.key = "Unknown".into();
        assert!(c.parse().is_err());
    }
    #[test]
    fn successful_switch_releases_old_and_conflicts_preserve_old() {
        let old = Shortcut::default().parse().unwrap();
        let mut c = Shortcut::default();
        c.shift = true;
        let new = c.parse().unwrap();
        let mock = Mock {
            keys: RefCell::new(vec![old]),
            conflict: Some(new),
            fail_release: None,
        };
        assert!(replace(&mock, Some(old), new).is_err());
        assert_eq!(*mock.keys.borrow(), vec![old]);
        let mock = Mock {
            keys: RefCell::new(vec![old]),
            conflict: None,
            fail_release: None,
        };
        replace(&mock, Some(old), new).unwrap();
        assert_eq!(*mock.keys.borrow(), vec![new]);
        replace(&mock, Some(new), new).unwrap();
        assert_eq!(mock.keys.borrow().len(), 1);
        let mock = Mock {
            keys: RefCell::new(vec![old]),
            conflict: None,
            fail_release: Some(old),
        };
        assert!(replace(&mock, Some(old), new).is_err());
        assert_eq!(*mock.keys.borrow(), vec![old]);
    }
}
