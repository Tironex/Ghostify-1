//! Global hotkeys: validation, storage and the actions they trigger.

use ghostify_core::{Error, Result};
use ghostify_db::repo::misc::{self, Hotkey};
use ghostify_db::Db;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyAction {
    PlayPause,
    Next,
    Previous,
    ToggleMiniPlayer,
}

impl HotkeyAction {
    pub const ALL: [HotkeyAction; 4] = [
        HotkeyAction::PlayPause,
        HotkeyAction::Next,
        HotkeyAction::Previous,
        HotkeyAction::ToggleMiniPlayer,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            HotkeyAction::PlayPause => "play_pause",
            HotkeyAction::Next => "next",
            HotkeyAction::Previous => "previous",
            HotkeyAction::ToggleMiniPlayer => "toggle_mini_player",
        }
    }

    pub fn parse(text: &str) -> Result<HotkeyAction> {
        HotkeyAction::ALL
            .into_iter()
            .find(|a| a.as_str() == text)
            .ok_or_else(|| Error::Invalid(format!("unknown hotkey action `{text}`")))
    }

    pub fn default_accelerator(self) -> &'static str {
        match self {
            HotkeyAction::PlayPause => "CommandOrControl+Alt+Space",
            HotkeyAction::Next => "CommandOrControl+Alt+Right",
            HotkeyAction::Previous => "CommandOrControl+Alt+Left",
            HotkeyAction::ToggleMiniPlayer => "CommandOrControl+Alt+M",
        }
    }
}

const MODIFIERS: [&str; 5] = ["CommandOrControl", "Control", "Super", "Alt", "Shift"];
const NAMED_KEYS: &[&str] = &[
    "Space",
    "Enter",
    "Tab",
    "Backspace",
    "Delete",
    "Insert",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Up",
    "Down",
    "Left",
    "Right",
];
/// Keys that make sense without a modifier.
const MEDIA_KEYS: &[&str] = &[
    "MediaPlayPause",
    "MediaNextTrack",
    "MediaPreviousTrack",
    "MediaStop",
];

fn modifier(token: &str) -> Option<&'static str> {
    match token.to_ascii_lowercase().as_str() {
        "commandorcontrol" | "cmdorctrl" => Some("CommandOrControl"),
        "ctrl" | "control" => Some("Control"),
        "super" | "meta" | "cmd" | "command" | "win" | "windows" => Some("Super"),
        "alt" | "option" => Some("Alt"),
        "shift" => Some("Shift"),
        _ => None,
    }
}

fn key(token: &str) -> Option<(String, bool)> {
    let lower = token.to_ascii_lowercase();
    if let Some(name) = NAMED_KEYS
        .iter()
        .chain(MEDIA_KEYS)
        .find(|k| k.to_ascii_lowercase() == lower)
    {
        return Some(((*name).to_owned(), MEDIA_KEYS.contains(name)));
    }
    if token.len() == 1 && token.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Some((token.to_ascii_uppercase(), false));
    }
    if let Some(number) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        if (1..=24).contains(&number) {
            return Some((format!("F{number}"), true));
        }
    }
    None
}

/// Returns the accelerator in the canonical spelling the shell registers
/// (`CommandOrControl+Alt+M`), or says what is wrong with it. Plain keys need a
/// modifier, otherwise the hotkey would block typing in every program.
pub fn validate_accelerator(text: &str) -> Result<String> {
    let mut modifiers: Vec<&'static str> = Vec::new();
    let mut main: Option<(String, bool)> = None;
    for token in text.split('+').map(str::trim) {
        if token.is_empty() {
            return Err(Error::Invalid(format!("`{text}` is not a valid shortcut")));
        }
        if let Some(m) = modifier(token) {
            if modifiers.contains(&m) {
                return Err(Error::Invalid(format!("`{text}` uses {m} twice")));
            }
            modifiers.push(m);
        } else if let Some(k) = key(token) {
            if main.replace(k).is_some() {
                return Err(Error::Invalid(format!("`{text}` has more than one key")));
            }
        } else {
            return Err(Error::Invalid(format!(
                "`{token}` is not a key Ghostify can use in a shortcut"
            )));
        }
    }
    let (main, standalone_ok) =
        main.ok_or_else(|| Error::Invalid(format!("`{text}` has no key")))?;
    if modifiers.is_empty() && !standalone_ok {
        return Err(Error::Invalid(format!(
            "`{text}` needs a modifier such as Ctrl or Alt"
        )));
    }
    modifiers.sort_by_key(|m| MODIFIERS.iter().position(|x| x == m));
    let mut parts: Vec<String> = modifiers.into_iter().map(str::to_owned).collect();
    parts.push(main);
    Ok(parts.join("+"))
}

pub struct HotkeyService {
    db: Db,
}

impl HotkeyService {
    pub fn new(db: Db) -> HotkeyService {
        HotkeyService { db }
    }

    /// All hotkeys; actions that have none yet get their default if it is free.
    pub fn list(&self) -> Result<Vec<Hotkey>> {
        self.db.with(|conn| {
            let existing = misc::list_hotkeys(conn)?;
            for action in HotkeyAction::ALL {
                if existing.iter().any(|h| h.action == action.as_str()) {
                    continue;
                }
                let taken = existing
                    .iter()
                    .any(|h| h.accelerator == action.default_accelerator());
                if !taken {
                    misc::set_hotkey(conn, action.as_str(), action.default_accelerator(), true)?;
                }
            }
            let all = misc::list_hotkeys(conn)?;
            Ok(all)
        })
    }

    pub fn set(&self, action: &str, accelerator: &str, enabled: bool) -> Result<Hotkey> {
        let action = HotkeyAction::parse(action)?;
        let accelerator = validate_accelerator(accelerator)?;
        self.list()?;
        self.db
            .with(|conn| misc::set_hotkey(conn, action.as_str(), &accelerator, enabled))
    }

    pub fn reset(&self, action: &str) -> Result<Hotkey> {
        let action = HotkeyAction::parse(action)?;
        self.set(action.as_str(), action.default_accelerator(), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings_are_normalised() {
        for (input, expected) in [
            ("ctrl+alt+m", "Control+Alt+M"),
            ("Alt + Ctrl + M", "Control+Alt+M"),
            ("cmdorctrl+shift+f5", "CommandOrControl+Shift+F5"),
            ("Super+Left", "Super+Left"),
            ("win+space", "Super+Space"),
            ("MediaPlayPause", "MediaPlayPause"),
            ("mediaNextTrack", "MediaNextTrack"),
            ("F12", "F12"),
            ("Ctrl+7", "Control+7"),
        ] {
            assert_eq!(validate_accelerator(input).unwrap(), expected, "{input}");
        }
    }

    #[test]
    fn invalid_shortcuts_are_explained() {
        for bad in [
            "",
            "+",
            "Ctrl+",
            "Ctrl+Alt",
            "Ctrl+Ctrl+A",
            "Ctrl+A+B",
            "A",
            "Space",
            "Left",
            "Ctrl+Banana",
            "Ctrl+F25",
            "Ctrl+F0",
            "Ctrl++A",
        ] {
            assert_eq!(
                validate_accelerator(bad).unwrap_err().code(),
                "invalid_input",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn defaults_are_valid_and_unique() {
        let mut seen = Vec::new();
        for action in HotkeyAction::ALL {
            let normalised = validate_accelerator(action.default_accelerator()).unwrap();
            assert_eq!(normalised, action.default_accelerator());
            assert!(!seen.contains(&normalised));
            seen.push(normalised);
            assert_eq!(HotkeyAction::parse(action.as_str()).unwrap(), action);
        }
        assert!(HotkeyAction::parse("launch_missiles").is_err());
    }

    #[test]
    fn defaults_are_created_once_and_can_be_changed_and_reset() {
        let svc = HotkeyService::new(Db::open_in_memory().unwrap());
        let list = svc.list().unwrap();
        assert_eq!(list.len(), 4);
        assert!(list.iter().all(|h| h.enabled));

        let changed = svc.set("next", "ctrl+shift+n", true).unwrap();
        assert_eq!(changed.accelerator, "Control+Shift+N");
        assert_eq!(
            svc.list()
                .unwrap()
                .iter()
                .find(|h| h.action == "next")
                .unwrap()
                .accelerator,
            "Control+Shift+N"
        );
        svc.set("next", "Control+Shift+N", false).unwrap();
        assert!(
            !svc.list()
                .unwrap()
                .iter()
                .find(|h| h.action == "next")
                .unwrap()
                .enabled
        );

        let reset = svc.reset("next").unwrap();
        assert_eq!(
            (reset.accelerator.as_str(), reset.enabled),
            ("CommandOrControl+Alt+Right", true)
        );
    }

    #[test]
    fn an_accelerator_can_belong_to_one_action_only() {
        let svc = HotkeyService::new(Db::open_in_memory().unwrap());
        assert_eq!(
            svc.set("next", "CommandOrControl+Alt+M", true)
                .unwrap_err()
                .code(),
            "conflict"
        );
        assert_eq!(
            svc.set("bogus", "Ctrl+A", true).unwrap_err().code(),
            "invalid_input"
        );
        assert_eq!(
            svc.set("next", "A", true).unwrap_err().code(),
            "invalid_input"
        );
    }

    #[test]
    fn a_taken_default_is_not_stolen() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| misc::set_hotkey(c, "play_pause", "CommandOrControl+Alt+M", true))
            .unwrap();
        let svc = HotkeyService::new(db);
        let list = svc.list().unwrap();
        assert!(
            list.iter().all(|h| h.action != "toggle_mini_player"),
            "its default is used by another action"
        );
        assert_eq!(list.len(), 3);
    }
}
