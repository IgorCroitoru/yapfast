//! The keys Yapfast watches, pressed in any app: the push-to-talk key, and the
//! keybinds that mute, deafen or do both. They come through the
//! GlobalShortcuts portal on Wayland, XInput 2's raw key events on X11, and a
//! low-level keyboard hook on Windows. Where none of these works, the keys
//! work while Yapfast's window has the focus, and settings say so. The
//! push-to-talk key tells [`yapfast_core::push_to_talk::hold`] when it goes
//! down or comes up; each of the others tells the handler given to
//! [`Watcher::start`] once per press.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering},
};

use serde::{Deserialize, Serialize};
use slint::winit_030::winit::{
    event::{ElementState, KeyEvent, WindowEvent},
    keyboard::{Key, KeyLocation, NamedKey, PhysicalKey},
    platform::scancode::PhysicalKeyExtScancode,
};
use yapfast_core::runtime::BackgroundRuntime;

/// A key recorded in settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keybind {
    /// winit's scan code for it: evdev's on Linux, which X11 reports 8
    /// higher, and set 1's on Windows, with 0xE000 for an extended key.
    pub scancode: u32,
    /// As settings show it, such as "V" or "Right Ctrl".
    pub name: String,
    /// Its XKB keysym name, such as "v" or "Control_R", which the
    /// GlobalShortcuts portal takes as the shortcut it would like.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
}

impl Keybind {
    /// The key a key press names, unless it has no scan code.
    pub fn recorded(event: &KeyEvent) -> Option<Self> {
        let scancode = event.physical_key.to_scancode()?;
        Some(Self {
            scancode,
            name: name(event),
            trigger: trigger(event),
        })
    }
}

/// The key without Shift or Caps Lock, so a letter reads as itself.
fn unmodified(event: &KeyEvent) -> Key {
    #[cfg(any(target_os = "linux", windows))]
    {
        use slint::winit_030::winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
        event.key_without_modifiers()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        event.logical_key.clone()
    }
}

fn name(event: &KeyEvent) -> String {
    let side = match event.location {
        KeyLocation::Left => "Left ",
        KeyLocation::Right => "Right ",
        KeyLocation::Numpad => "Numpad ",
        KeyLocation::Standard => "",
    };
    match unmodified(event) {
        Key::Character(text) => format!("{side}{}", text.to_uppercase()),
        Key::Named(named) => {
            let named = match named {
                NamedKey::Control => "Ctrl".to_owned(),
                NamedKey::Super | NamedKey::Meta => "Super".to_owned(),
                NamedKey::ArrowUp => "Up".to_owned(),
                NamedKey::ArrowDown => "Down".to_owned(),
                NamedKey::ArrowLeft => "Left".to_owned(),
                NamedKey::ArrowRight => "Right".to_owned(),
                NamedKey::Space => "Space".to_owned(),
                named => format!("{named:?}"),
            };
            format!("{side}{named}")
        }
        _ => match event.physical_key {
            PhysicalKey::Code(code) => format!("{code:?}"),
            PhysicalKey::Unidentified(_) => "Unknown key".to_owned(),
        },
    }
}

/// The key's keysym name, as the XDG shortcuts specification writes a
/// trigger; on Linux, where only the portal needs it.
#[cfg(target_os = "linux")]
fn trigger(event: &KeyEvent) -> Option<String> {
    let right = event.location == KeyLocation::Right;
    let side =
        |left: &str, right_name: &str| Some(if right { right_name } else { left }.to_owned());
    match unmodified(event) {
        Key::Character(text) => {
            let mut chars = text.chars();
            let (Some(char), None) = (chars.next(), chars.next()) else {
                return None;
            };
            let name = xkeysym::Keysym::from_char(char).name()?;
            Some(name.strip_prefix("XK_").unwrap_or(name).to_owned())
        }
        Key::Named(named) => match named {
            NamedKey::Control => side("Control_L", "Control_R"),
            NamedKey::Shift => side("Shift_L", "Shift_R"),
            NamedKey::Alt => side("Alt_L", "Alt_R"),
            NamedKey::Super | NamedKey::Meta => side("Super_L", "Super_R"),
            NamedKey::AltGraph => Some("ISO_Level3_Shift".to_owned()),
            NamedKey::CapsLock => Some("Caps_Lock".to_owned()),
            NamedKey::ScrollLock => Some("Scroll_Lock".to_owned()),
            NamedKey::NumLock => Some("Num_Lock".to_owned()),
            NamedKey::PageUp => Some("Prior".to_owned()),
            NamedKey::PageDown => Some("Next".to_owned()),
            NamedKey::Space => Some("space".to_owned()),
            NamedKey::Enter => Some("Return".to_owned()),
            NamedKey::Backspace => Some("BackSpace".to_owned()),
            NamedKey::ArrowUp => Some("Up".to_owned()),
            NamedKey::ArrowDown => Some("Down".to_owned()),
            NamedKey::ArrowLeft => Some("Left".to_owned()),
            NamedKey::ArrowRight => Some("Right".to_owned()),
            NamedKey::Tab
            | NamedKey::Insert
            | NamedKey::Delete
            | NamedKey::Home
            | NamedKey::End
            | NamedKey::Pause => Some(format!("{named:?}")),
            NamedKey::ContextMenu => Some("Menu".to_owned()),
            named => {
                let name = format!("{named:?}");
                // F1 to F35 are their own keysyms.
                (name.starts_with('F') && name[1..].parse::<u8>().is_ok()).then_some(name)
            }
        },
        _ => None,
    }
}

#[cfg(not(target_os = "linux"))]
fn trigger(_event: &KeyEvent) -> Option<String> {
    None
}

/// What a key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Sends the microphone while held, in push to talk.
    PushToTalk,
    /// Mutes the microphone, or unmutes it.
    ToggleMute,
    /// Deafens, or undeafens.
    ToggleDeafen,
    /// Mutes the microphone and deafens, or brings both back.
    ToggleMuteAll,
}

impl Action {
    pub const ALL: [Action; 4] = [
        Action::PushToTalk,
        Action::ToggleMute,
        Action::ToggleDeafen,
        Action::ToggleMuteAll,
    ];

    /// The keybinds that act once per press, in the order settings list them.
    pub const TOGGLES: [Action; 3] = [
        Action::ToggleMute,
        Action::ToggleDeafen,
        Action::ToggleMuteAll,
    ];

    fn index(self) -> usize {
        self as usize
    }
}

/// The shortcuts a desktop gave, in its words, by what they do; empty
/// where it hasn't allowed one yet.
pub type Shortcuts = Vec<(Action, String)>;

/// Where the keys work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Asking the desktop for them.
    Asking,
    /// In every app; with the shortcuts the desktop gave them, in its own
    /// words, when it says.
    Everywhere(Option<Shortcuts>),
    /// Only while Yapfast's window has the focus, for this reason, which
    /// the note goes on from.
    Focused(&'static str),
}

impl Reach {
    /// What settings say under the push-to-talk key.
    pub fn note(&self) -> String {
        match self {
            Reach::Asking => "Asking your desktop to let Yapfast use the key in every app...".into(),
            Reach::Everywhere(None) => "Hold the key in any app to talk.".into(),
            Reach::Everywhere(Some(shortcuts)) => {
                match shortcuts.iter().find(|(action, _)| *action == Action::PushToTalk) {
                    Some((_, shortcut)) if !shortcut.is_empty() => format!(
                        "Your desktop gave push to talk the shortcut {shortcut}. Hold it in any app to talk; change it in your system's global shortcuts settings."
                    ),
                    _ => "Your desktop hasn't allowed this shortcut yet. Approve it in your system's global shortcuts settings.".into(),
                }
            }
            Reach::Focused(why) => format!(
                "{why}, so push to talk works only while Yapfast's window is in front."
            ),
        }
    }

    /// What Keybinds say under the keys that mute and deafen.
    pub fn keybinds_note(&self) -> String {
        match self {
            Reach::Asking => "Asking your desktop to let Yapfast use these keys in every app...".into(),
            Reach::Everywhere(None) => "Your keybinds work in any app.".into(),
            Reach::Everywhere(Some(shortcuts)) => {
                let toggles: Vec<_> = shortcuts
                    .iter()
                    .filter(|(action, _)| *action != Action::PushToTalk)
                    .collect();
                if toggles.is_empty() || toggles.iter().any(|(_, shortcut)| shortcut.is_empty()) {
                    "Your desktop hasn't allowed these shortcuts yet. Approve them in your system's global shortcuts settings.".into()
                } else {
                    "Your desktop manages these shortcuts and may show them differently. Change them in your system's global shortcuts settings.".into()
                }
            }
            Reach::Focused(why) => format!(
                "{why}, so keybinds work only while Yapfast's window is in front."
            ),
        }
    }
}

const NONE: u32 = u32::MAX;
/// The scan code of the key each [`Action`] has while it's watched; [`NONE`]
/// otherwise. Atomics, as a keyboard hook reads them and mustn't wait.
static KEYS: [AtomicU32; 4] = [
    AtomicU32::new(NONE),
    AtomicU32::new(NONE),
    AtomicU32::new(NONE),
    AtomicU32::new(NONE),
];
/// Whether each action's key is down, so a held key's repeats act once.
static DOWN: [AtomicBool; 4] = [
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
];
/// Whether the window's own key events stand in for global keys.
static FOCUSED: AtomicBool = AtomicBool::new(false);
/// The action whose key the window's latest key press was, as an index into
/// [`Action::ALL`]; [`NONE`] if it wasn't one.
static PRESSED: AtomicU32 = AtomicU32::new(NONE);

type Handler = Arc<dyn Fn(Action) + Send + Sync>;
/// What hears each press of a keybind other than push to talk.
static HANDLER: Mutex<Option<Handler>> = Mutex::new(None);

fn bind(action: Action, scancode: Option<u32>) {
    KEYS[action.index()].store(scancode.unwrap_or(NONE), Ordering::Relaxed);
}

/// The action a key with this scan code does, if it's watched.
fn action_for(scancode: u32) -> Option<Action> {
    if scancode == NONE {
        return None;
    }
    Action::ALL
        .into_iter()
        .find(|action| KEYS[action.index()].load(Ordering::Relaxed) == scancode)
}

/// A watched key went down or came up. Push to talk follows the key; the
/// other keybinds act as it goes down, and not again until it comes up.
fn press(action: Action, down: bool) {
    if action == Action::PushToTalk {
        yapfast_core::push_to_talk::hold(down);
        return;
    }
    if DOWN[action.index()].swap(down, Ordering::Relaxed) != down && down {
        let handler = HANDLER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(handler) = handler {
            handler(action);
        }
    }
}

fn release_all() {
    for action in Action::ALL {
        press(action, false);
    }
}

/// The action whose key the key press Slint is handling is, which shouldn't
/// also type into the composer.
pub fn pressed_key() -> Option<Action> {
    Action::ALL
        .get(PRESSED.load(Ordering::Relaxed) as usize)
        .copied()
}

/// Presses the keys from the window's own events while no global key works.
/// Losing the focus lets them go, as their release won't come.
pub fn window_event(event: &WindowEvent) {
    if let WindowEvent::KeyboardInput { event, .. } = event
        && event.state == ElementState::Pressed
    {
        let action = event.physical_key.to_scancode().and_then(action_for);
        PRESSED.store(
            action.map_or(NONE, |action| action.index() as u32),
            Ordering::Relaxed,
        );
    }
    if !FOCUSED.load(Ordering::Relaxed) {
        return;
    }
    match event {
        WindowEvent::KeyboardInput { event, .. } if !event.repeat => {
            if let Some(action) = event.physical_key.to_scancode().and_then(action_for) {
                press(action, event.state == ElementState::Pressed);
            }
        }
        WindowEvent::Focused(false) => release_all(),
        _ => {}
    }
}

/// Watches the keys in every app it can, until dropped.
pub struct Watcher {
    keys: Vec<(Action, Keybind)>,
    reach: Arc<Mutex<Reach>>,
    #[cfg(target_os = "linux")]
    _global: Option<linux::Global>,
    #[cfg(windows)]
    _global: Option<windows::Hook>,
}

impl Watcher {
    /// Starts watching `keys`; `told` hears where they work whenever that
    /// changes, from any thread, and `pressed` each press of one that isn't
    /// push to talk.
    pub fn start(
        keys: Vec<(Action, Keybind)>,
        runtime: &BackgroundRuntime,
        told: impl Fn(Reach) + Send + Sync + 'static,
        pressed: impl Fn(Action) + Send + Sync + 'static,
    ) -> Self {
        let reach = Arc::new(Mutex::new(Reach::Asking));
        let shared = reach.clone();
        for (action, key) in &keys {
            bind(*action, Some(key.scancode));
        }
        *HANDLER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(pressed));
        let report = Arc::new(move |now: Reach| {
            FOCUSED.store(matches!(now, Reach::Focused(_)), Ordering::Relaxed);
            *shared
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = now.clone();
            told(now);
        });
        #[cfg(target_os = "linux")]
        let global = linux::Global::start(&keys, runtime, report);
        #[cfg(windows)]
        let global = {
            let _ = runtime;
            windows::Hook::start(report)
        };
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = runtime;
            report(Reach::Focused(
                "Yapfast can't watch keys in other apps on this system yet",
            ));
        }
        Self {
            keys,
            reach,
            #[cfg(any(target_os = "linux", windows))]
            _global: global,
        }
    }

    /// The keys it watches, by what they do.
    pub fn keys(&self) -> &[(Action, Keybind)] {
        &self.keys
    }

    pub fn reach(&self) -> Reach {
        self.reach
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        FOCUSED.store(false, Ordering::Relaxed);
        for action in Action::ALL {
            bind(action, None);
        }
        release_all();
        *HANDLER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

type Report = Arc<dyn Fn(Reach) + Send + Sync>;

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    use anyhow::{Context, Result, anyhow, bail};
    use futures_util::StreamExt;
    use x11rb::{
        connection::Connection as _,
        protocol::{
            Event,
            xinput::{self, ConnectionExt as _},
            xproto::{self, ConnectionExt as _},
        },
        rust_connection::RustConnection,
    };
    use yapfast_core::runtime::{BackgroundRuntime, BackgroundTask};
    use zbus::{
        Connection, MessageStream,
        zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
    };

    use super::{Action, Keybind, Reach, Report};

    /// A global key on Wayland or X11; stops when dropped.
    pub enum Global {
        Portal { _session: BackgroundTask<()> },
        X11 { _events: X11 },
    }

    impl Global {
        /// Wayland's desktops grab keys through the portal; X11 lets any
        /// client hear every key.
        pub fn start(
            keys: &[(Action, Keybind)],
            runtime: &BackgroundRuntime,
            report: Report,
        ) -> Option<Self> {
            let wayland =
                std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
            if wayland {
                report(Reach::Asking);
                let wanted: Vec<_> = keys
                    .iter()
                    .map(|(action, key)| (*action, key.trigger.clone()))
                    .collect();
                return Some(Global::Portal {
                    _session: runtime.spawn(async move {
                        if let Err(error) = portal(wanted, &report).await {
                            tracing::info!(
                                error = format!("{error:#}"),
                                "no global keys through the portal"
                            );
                            report(Reach::Focused(
                                "Your desktop doesn't offer Yapfast a global shortcut",
                            ));
                        }
                    }),
                });
            }
            match X11::start(report.clone()) {
                Ok(x11) => {
                    report(Reach::Everywhere(None));
                    Some(Global::X11 { _events: x11 })
                }
                Err(error) => {
                    tracing::info!(error = format!("{error:#}"), "no global keys on X11");
                    report(Reach::Focused("Yapfast couldn't watch keys in other apps"));
                    None
                }
            }
        }
    }

    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";

    /// The id the portal knows an action's shortcut by.
    fn shortcut_id(action: Action) -> &'static str {
        match action {
            Action::PushToTalk => "push-to-talk",
            Action::ToggleMute => "toggle-mute",
            Action::ToggleDeafen => "toggle-deafen",
            Action::ToggleMuteAll => "toggle-mute-all",
        }
    }

    fn shortcut_action(id: &str) -> Option<Action> {
        Action::ALL
            .into_iter()
            .find(|action| shortcut_id(*action) == id)
    }

    /// What the desktop shows a person for an action's shortcut.
    fn shortcut_description(action: Action) -> &'static str {
        match action {
            Action::PushToTalk => "Push to talk",
            Action::ToggleMute => "Toggle mute",
            Action::ToggleDeafen => "Toggle deafen",
            Action::ToggleMuteAll => "Toggle mute and deafen",
        }
    }

    /// Binds the keys through the GlobalShortcuts portal and presses them as
    /// the desktop says, until the task is dropped, which ends the session
    /// with the connection.
    async fn portal(wanted: Vec<(Action, Option<String>)>, report: &Report) -> Result<()> {
        let connection = Connection::session().await?;
        // Names Yapfast to the portal, which outside a sandbox can't tell
        // the app otherwise; older portals don't have it.
        let _ = connection
            .call_method(
                Some(PORTAL),
                PATH,
                Some("org.freedesktop.host.portal.Registry"),
                "Register",
                &(crate::icon::APP_ID, HashMap::<&str, Value>::new()),
            )
            .await;
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(PORTAL)?
            .build();
        let mut signals = MessageStream::for_match_rule(rule, &connection, Some(16)).await?;
        let sender = connection
            .unique_name()
            .context("no name on the session bus")?
            .trim_start_matches(':')
            .replace('.', "_");
        let token = format!("yapfast_{}", std::process::id());
        let request = |step: &str| format!("{PATH}/request/{sender}/{token}_{step}");

        let options = HashMap::from([
            ("handle_token", Value::from(format!("{token}_session"))),
            ("session_handle_token", Value::from(token.clone())),
        ]);
        connection
            .call_method(
                Some(PORTAL),
                PATH,
                Some(SHORTCUTS),
                "CreateSession",
                &(options,),
            )
            .await?;
        let results = response(&mut signals, &request("session")).await?;
        let session = match results.get("session_handle").map(|value| &**value) {
            Some(Value::Str(handle)) => OwnedObjectPath::try_from(handle.as_str())?,
            Some(Value::ObjectPath(handle)) => handle.clone().into(),
            _ => bail!("the portal gave no session"),
        };

        let shortcuts: Vec<(&str, HashMap<&str, Value>)> = wanted
            .iter()
            .map(|(action, trigger)| {
                let mut details =
                    HashMap::from([("description", Value::from(shortcut_description(*action)))]);
                if let Some(trigger) = trigger {
                    details.insert("preferred_trigger", Value::from(trigger.as_str()));
                }
                (shortcut_id(*action), details)
            })
            .collect();
        let options = HashMap::from([("handle_token", Value::from(format!("{token}_bind")))]);
        connection
            .call_method(
                Some(PORTAL),
                PATH,
                Some(SHORTCUTS),
                "BindShortcuts",
                &(&session, shortcuts, "", options),
            )
            .await?;
        let results = response(&mut signals, &request("bind")).await?;
        report(Reach::Everywhere(Some(described(&results))));

        while let Some(message) = signals.next().await {
            let message = message?;
            let header = message.header();
            let held = match header.member().map(|member| member.as_str()) {
                Some("Activated") => true,
                Some("Deactivated") => false,
                Some("ShortcutsChanged") => {
                    if let Ok((path, shortcuts)) = message
                        .body()
                        .deserialize::<(ObjectPath, Vec<(String, HashMap<String, OwnedValue>)>)>()
                        && path == *session
                    {
                        report(Reach::Everywhere(Some(trigger_descriptions(&shortcuts))));
                    }
                    continue;
                }
                Some("Closed") if header.path().is_some_and(|path| *path == *session) => {
                    bail!("the desktop closed the shortcut's session");
                }
                _ => continue,
            };
            if let Ok((path, id, ..)) =
                message
                    .body()
                    .deserialize::<(ObjectPath, &str, u64, HashMap<String, OwnedValue>)>()
                && path == *session
                && let Some(action) = shortcut_action(id)
            {
                super::press(action, held);
            }
        }
        Err(anyhow!("the session bus closed"))
    }

    /// Waits for the portal's answer to a request, at its object path.
    async fn response(
        signals: &mut MessageStream,
        request: &str,
    ) -> Result<HashMap<String, OwnedValue>> {
        while let Some(message) = signals.next().await {
            let message = message?;
            let header = message.header();
            if header
                .member()
                .is_none_or(|member| member.as_str() != "Response")
                || header.path().is_none_or(|path| path.as_str() != request)
            {
                continue;
            }
            let (code, results) = message
                .body()
                .deserialize::<(u32, HashMap<String, OwnedValue>)>()?;
            return match code {
                0 => Ok(results),
                1 => Err(anyhow!("the shortcut was declined")),
                _ => Err(anyhow!("the portal failed ({code})")),
            };
        }
        Err(anyhow!("the session bus closed"))
    }

    /// The shortcuts the desktop bound, in its words; empty when it bound
    /// none yet.
    fn described(results: &HashMap<String, OwnedValue>) -> super::Shortcuts {
        results
            .get("shortcuts")
            .and_then(|value| {
                Vec::<(String, HashMap<String, OwnedValue>)>::try_from(value.try_clone().ok()?).ok()
            })
            .map(|shortcuts| trigger_descriptions(&shortcuts))
            .unwrap_or_default()
    }

    /// Each of Yapfast's shortcuts among them, with the trigger the desktop
    /// describes for it, or none while it has no trigger.
    fn trigger_descriptions(
        shortcuts: &[(String, HashMap<String, OwnedValue>)],
    ) -> super::Shortcuts {
        shortcuts
            .iter()
            .filter_map(|(id, details)| {
                let action = shortcut_action(id)?;
                let text = match details.get("trigger_description").map(|value| &**value) {
                    Some(Value::Str(text)) => text.to_string(),
                    _ => String::new(),
                };
                Some((action, text))
            })
            .collect()
    }

    /// XInput 2's raw key events from the root window, which come whatever
    /// window has the focus and leave the key to it. A thread waits on them.
    pub struct X11 {
        connection: Arc<RustConnection>,
        window: xproto::Window,
        stop: Arc<AtomicBool>,
    }

    impl X11 {
        fn start(report: Report) -> Result<Self> {
            let (connection, screen) = x11rb::connect(None)?;
            let root = connection.setup().roots[screen].root;
            let version = connection.xinput_xi_query_version(2, 2)?.reply()?;
            if version.major_version < 2 {
                bail!("the X server has no XInput 2");
            }
            // A window of its own to wake the thread with when stopping.
            let window = connection.generate_id()?;
            connection.create_window(
                0,
                window,
                root,
                0,
                0,
                1,
                1,
                0,
                xproto::WindowClass::INPUT_ONLY,
                0,
                &xproto::CreateWindowAux::new(),
            )?;
            connection.xinput_xi_select_events(
                root,
                &[xinput::EventMask {
                    deviceid: xinput::Device::ALL_MASTER.into(),
                    mask: vec![
                        xinput::XIEventMask::RAW_KEY_PRESS | xinput::XIEventMask::RAW_KEY_RELEASE,
                    ],
                }],
            )?;
            connection.flush()?;
            let connection = Arc::new(connection);
            let stop = Arc::new(AtomicBool::new(false));
            let (waiting, stopped) = (connection.clone(), stop.clone());
            std::thread::Builder::new()
                .name("yapfast-keys".into())
                .spawn(move || {
                    // X11 keycodes are evdev's plus 8.
                    let action = |keycode: u32| keycode.checked_sub(8).and_then(super::action_for);
                    while !stopped.load(Ordering::Relaxed) {
                        match waiting.wait_for_event() {
                            Ok(Event::XinputRawKeyPress(event)) => {
                                if let Some(action) = action(event.detail) {
                                    super::press(action, true);
                                }
                            }
                            Ok(Event::XinputRawKeyRelease(event)) => {
                                if let Some(action) = action(event.detail) {
                                    super::press(action, false);
                                }
                            }
                            Ok(_) => {}
                            Err(error) => {
                                tracing::warn!(%error, "lost the X server's keys");
                                super::release_all();
                                report(Reach::Focused("Yapfast lost the X server's keys"));
                                break;
                            }
                        }
                    }
                })?;
            Ok(Self {
                connection,
                window,
                stop,
            })
        }
    }

    impl Drop for X11 {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let wake =
                xproto::ClientMessageEvent::new(32, self.window, xproto::AtomEnum::NONE, [0u32; 5]);
            let _ =
                self.connection
                    .send_event(false, self.window, xproto::EventMask::NO_EVENT, wake);
            let _ = self.connection.flush();
        }
    }

    #[cfg(test)]
    mod tests {
        use std::time::{Duration, Instant};

        use super::*;

        /// Waits up to two seconds for the key to be `held` or not.
        fn wait_held(held: bool) {
            let until = Instant::now() + Duration::from_secs(2);
            while yapfast_core::push_to_talk::held() != held {
                assert!(Instant::now() < until, "the key wasn't held: {held}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        /// Pressing the key in another client holds it. Needs `DISPLAY` on a
        /// private X server, such as `Xvfb :57`, and `xdotool`, which types
        /// through XTEST as a keyboard would.
        #[test]
        #[ignore = "needs DISPLAY on a private X server and xdotool"]
        fn x11_hears_the_key_whatever_has_the_focus() {
            let x11 = {
                super::super::bind(Action::PushToTalk, Some(47)); // evdev's V
                X11::start(Arc::new(|_| {})).unwrap()
            };
            let xdotool = |action| {
                let status = std::process::Command::new("xdotool")
                    .args([action, "v"])
                    .status()
                    .unwrap();
                assert!(status.success());
            };
            xdotool("keydown");
            wait_held(true);
            xdotool("keyup");
            wait_held(false);
            // Another key leaves it alone.
            std::process::Command::new("xdotool")
                .args(["key", "b"])
                .status()
                .unwrap();
            std::thread::sleep(Duration::from_millis(100));
            assert!(!yapfast_core::push_to_talk::held());
            drop(x11);
        }

        /// A stand-in for xdg-desktop-portal's GlobalShortcuts: it answers
        /// each request with Response on the request's path, binds the
        /// shortcut to what was preferred, and then presses it.
        struct FakePortal;

        #[zbus::interface(name = "org.freedesktop.portal.GlobalShortcuts")]
        impl FakePortal {
            async fn create_session(
                &self,
                options: HashMap<String, OwnedValue>,
                #[zbus(header)] header: zbus::message::Header<'_>,
                #[zbus(connection)] connection: &Connection,
            ) -> OwnedObjectPath {
                let request = request_path(&header, &options);
                let session = format!("{PATH}/session/test/1");
                let results = HashMap::from([("session_handle", Value::from(session))]);
                respond(connection, &request, results).await;
                request
            }

            async fn bind_shortcuts(
                &self,
                session: OwnedObjectPath,
                shortcuts: Vec<(String, HashMap<String, OwnedValue>)>,
                _parent_window: String,
                options: HashMap<String, OwnedValue>,
                #[zbus(header)] header: zbus::message::Header<'_>,
                #[zbus(connection)] connection: &Connection,
            ) -> OwnedObjectPath {
                let request = request_path(&header, &options);
                let (id, details) = &shortcuts[0];
                let Some(Value::Str(trigger)) = details.get("preferred_trigger").map(|v| &**v)
                else {
                    panic!("no preferred trigger");
                };
                let bound = vec![(
                    id.clone(),
                    HashMap::from([(
                        "trigger_description".to_owned(),
                        Value::from(trigger.to_uppercase()),
                    )]),
                )];
                respond(
                    connection,
                    &request,
                    HashMap::from([("shortcuts", Value::from(bound))]),
                )
                .await;
                let connection = connection.clone();
                let (session, id) = (session.clone(), id.clone());
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(100));
                    zbus::block_on(press(&connection, &session, &id, "Activated"));
                });
                request
            }
        }

        fn request_path(
            header: &zbus::message::Header<'_>,
            options: &HashMap<String, OwnedValue>,
        ) -> OwnedObjectPath {
            let sender = header
                .sender()
                .unwrap()
                .trim_start_matches(':')
                .replace('.', "_");
            let Some(Value::Str(token)) = options.get("handle_token").map(|v| &**v) else {
                panic!("no handle token");
            };
            OwnedObjectPath::try_from(format!("{PATH}/request/{sender}/{token}")).unwrap()
        }

        async fn respond(
            connection: &Connection,
            request: &OwnedObjectPath,
            results: HashMap<&str, Value<'_>>,
        ) {
            connection
                .emit_signal(
                    None::<()>,
                    request,
                    "org.freedesktop.portal.Request",
                    "Response",
                    &(0u32, results),
                )
                .await
                .unwrap();
        }

        async fn press(connection: &Connection, session: &OwnedObjectPath, id: &str, signal: &str) {
            let options = HashMap::<&str, Value>::new();
            connection
                .emit_signal(
                    None::<()>,
                    PATH,
                    SHORTCUTS,
                    signal,
                    &(session, id, 0u64, options),
                )
                .await
                .unwrap();
        }

        /// The portal's handshake, and its shortcut going down and up. Needs
        /// a private session bus, as `dbus-run-session` gives.
        #[test]
        #[ignore = "needs a private session bus"]
        fn the_portal_binds_the_key_and_holds_it() {
            let server = zbus::blocking::connection::Builder::session()
                .unwrap()
                .name(PORTAL)
                .unwrap()
                .serve_at(PATH, FakePortal)
                .unwrap()
                .build()
                .unwrap();
            let told = Arc::new(std::sync::Mutex::new(Vec::new()));
            let heard = told.clone();
            let report: Report = Arc::new(move |reach| heard.lock().unwrap().push(reach));
            let runtime = BackgroundRuntime::new().unwrap();
            let wanted = vec![(Action::PushToTalk, Some("v".to_owned()))];
            let _task = runtime.spawn(async move { portal(wanted, &report).await });
            let until = Instant::now() + Duration::from_secs(5);
            while !yapfast_core::push_to_talk::held() {
                assert!(
                    Instant::now() < until,
                    "the shortcut wasn't held: {:?}",
                    told.lock().unwrap()
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                *told.lock().unwrap(),
                [Reach::Everywhere(Some(vec![(Action::PushToTalk, "V".into())]))]
            );
            let session = OwnedObjectPath::try_from(format!("{PATH}/session/test/1")).unwrap();
            let id = shortcut_id(Action::PushToTalk);
            zbus::block_on(press(server.inner(), &session, id, "Deactivated"));
            wait_held(false);
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::{ptr, sync::mpsc};

    use windows_sys::Win32::{
        Foundation::{LPARAM, LRESULT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG,
            PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN,
            WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
        },
    };

    use super::{Reach, Report};

    /// A low-level keyboard hook on a thread of its own, which sees every
    /// key, its release included, and passes each on. Removed when dropped.
    pub struct Hook {
        thread: u32,
    }

    impl Hook {
        pub fn start(report: Report) -> Option<Self> {
            let (started, thread) = mpsc::sync_channel(1);
            let spawned = std::thread::Builder::new()
                .name("yapfast-keys".into())
                .spawn(move || {
                    // SAFETY: the hook procedure below only reads the event
                    // Windows hands it; the message loop runs on this thread,
                    // which the hook needs, until WM_QUIT.
                    unsafe {
                        let hook = SetWindowsHookExW(
                            WH_KEYBOARD_LL,
                            Some(proc),
                            GetModuleHandleW(ptr::null()),
                            0,
                        );
                        if hook.is_null() {
                            let _ = started.send(None);
                            return;
                        }
                        let _ = started.send(Some(GetCurrentThreadId()));
                        let mut message: MSG = std::mem::zeroed();
                        while GetMessageW(&mut message, ptr::null_mut(), 0, 0) > 0 {}
                        UnhookWindowsHookEx(hook);
                    }
                });
            let thread = spawned.ok().and_then(|_| thread.recv().ok().flatten());
            match thread {
                Some(thread) => {
                    report(Reach::Everywhere(None));
                    Some(Self { thread })
                }
                None => {
                    report(Reach::Focused(
                        "Windows didn't let Yapfast watch keys in other apps",
                    ));
                    None
                }
            }
        }
    }

    impl Drop for Hook {
        fn drop(&mut self) {
            // SAFETY: posting to a thread id is harmless if it has ended.
            unsafe {
                PostThreadMessageW(self.thread, WM_QUIT, 0, 0);
            }
        }
    }

    /// Notes a watched key going down or up, and passes every key on.
    unsafe extern "system" fn proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            // SAFETY: for HC_ACTION, lparam points at the key's details.
            let key = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            let extended = if key.flags & LLKHF_EXTENDED != 0 {
                0xE000
            } else {
                0
            };
            if let Some(action) = super::action_for(key.scanCode | extended) {
                match wparam as u32 {
                    WM_KEYDOWN | WM_SYSKEYDOWN => super::press(action, true),
                    WM_KEYUP | WM_SYSKEYUP => super::press(action, false),
                    _ => {}
                }
            }
        }
        // SAFETY: the next hook gets the event as it came.
        unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
    }

    #[cfg(test)]
    mod tests {
        use std::{
            sync::Arc,
            time::{Duration, Instant},
        };

        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
            SendInput,
        };

        use super::*;

        /// Types a key by its scan code, as a keyboard would.
        fn send(scancode: u16, up: bool) {
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: scancode,
                        dwFlags: KEYEVENTF_SCANCODE | if up { KEYEVENTF_KEYUP } else { 0 },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            // SAFETY: one input, of the size Windows expects.
            let sent = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
            assert_eq!(sent, 1);
        }

        fn wait_held(held: bool) {
            let until = Instant::now() + Duration::from_secs(2);
            while yapfast_core::push_to_talk::held() != held {
                assert!(Instant::now() < until, "the key wasn't held: {held}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        /// The hook hears the key typed anywhere, down and up. Run it alone,
        /// on a desktop, or under Wine with an X server.
        #[test]
        #[ignore = "types into the desktop"]
        fn the_hook_hears_the_key_go_down_and_up() {
            crate::push_to_talk::bind(crate::push_to_talk::Action::PushToTalk, Some(0x2F)); // V
            let hook = Hook::start(Arc::new(|_| {})).expect("no hook");
            send(0x2F, false);
            wait_held(true);
            send(0x2F, true);
            wait_held(false);
            send(0x30, false); // B
            send(0x30, true);
            std::thread::sleep(Duration::from_millis(100));
            assert!(!yapfast_core::push_to_talk::held());
            drop(hook);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// A key held down presses its keybind once, and again after it comes up.
    #[test]
    fn a_held_keybind_acts_once_per_press() {
        let count = Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        *HANDLER.lock().unwrap() = Some(Arc::new(move |action| {
            assert_eq!(action, Action::ToggleMute);
            counted.fetch_add(1, Ordering::Relaxed);
        }));
        bind(Action::ToggleMute, Some(7));
        assert_eq!(action_for(7), Some(Action::ToggleMute));
        assert_eq!(action_for(8), None);
        assert_eq!(action_for(NONE), None);
        press(Action::ToggleMute, true);
        press(Action::ToggleMute, true);
        assert_eq!(count.load(Ordering::Relaxed), 1, "its repeats act once");
        press(Action::ToggleMute, false);
        assert_eq!(count.load(Ordering::Relaxed), 1, "letting go acts on nothing");
        press(Action::ToggleMute, true);
        assert_eq!(count.load(Ordering::Relaxed), 2);
        press(Action::ToggleMute, false);
        bind(Action::ToggleMute, None);
        assert_eq!(action_for(7), None);
        *HANDLER.lock().unwrap() = None;
    }

    #[test]
    fn notes_tell_what_the_desktop_allowed() {
        let given = Reach::Everywhere(Some(vec![
            (Action::PushToTalk, "V".into()),
            (Action::ToggleMute, String::new()),
        ]));
        assert!(given.note().contains("gave push to talk the shortcut V"));
        assert!(given.keybinds_note().contains("hasn't allowed"));
        let waiting = Reach::Everywhere(Some(vec![(Action::ToggleMute, "F9".into())]));
        assert!(waiting.note().contains("hasn't allowed"));
        assert!(waiting.keybinds_note().contains("manages these shortcuts"));
        assert!(Reach::Everywhere(None).keybinds_note().contains("any app"));
    }
}
