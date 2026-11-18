//! The notification area icon (the menu bar item on macOS) that switches accounts.
//!
//! The menu lists the saved accounts with the time each was last used, checks the signed-in one, and greys out
//! the ones that need a new sign-in. Choosing an account starts `claude-account-sync switch <account> --gui` in
//! the background, which asks the same questions as the picker. The menu also opens the picker, signs in to
//! another account, and opens the agent's log.
//!
//! The menu is rebuilt when what it shows changes. The tray checks every [`REFRESH`], because the watcher, a
//! switch, or the app can change the profiles and the sign-in at any time.
//!
//! The tray runs the platform's event loop on the calling thread, which must be the main thread on macOS.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::profiles::picker::{self, NEW_SIGN_IN, PickerRow};
use crate::profiles::{LoginState, ProfileStore, spawn_cli};
use crate::state::StateDir;

/// How often the tray checks whether the menu needs to change.
const REFRESH: Duration = Duration::from_secs(5);

/// The tray icon: the app icon on Windows, a template image that macOS tints for the menu bar.
#[cfg(not(target_os = "macos"))]
const ICON_PNG: &[u8] = include_bytes!("../assets/png/icon-32.png");
#[cfg(target_os = "macos")]
const ICON_PNG: &[u8] = include_bytes!("../assets/png/tray-template-44.png");

const ID_PICKER: &str = "picker";
const ID_NEW: &str = "new";
const ID_LOG: &str = "log";
const ID_QUIT: &str = "quit";
const ACCOUNT_PREFIX: &str = "account:";

/// Options of [`run`].
#[derive(Clone, Debug, Default)]
pub struct TrayOptions {
    /// Passes `--dry-run` to every switch that the menu starts, so that nothing changes.
    pub dry_run: bool,
}

enum UserEvent {
    Menu(MenuEvent),
}

/// What the menu shows. The menu is rebuilt only when this changes.
#[derive(Clone, PartialEq, Eq)]
struct MenuState {
    current: Option<String>,
    rows: Vec<PickerRow>,
}

impl MenuState {
    fn read(data_dir: &Path, state_dir: &StateDir) -> Self {
        let login = LoginState::read(data_dir);
        let profiles = ProfileStore::new(&state_dir.root)
            .list()
            .unwrap_or_default();
        let rows = picker::rows(&profiles, &login, SystemTime::now());
        let current = login.signed_in_account().map(|account| {
            profiles
                .iter()
                .find(|profile| profile.account == account)
                .map_or_else(
                    || format!("account {}", &account[..8]),
                    |profile| profile.display_name(),
                )
        });
        Self { current, rows }
    }

    fn tooltip(&self) -> String {
        match &self.current {
            Some(name) => format!("Claude account: {name}"),
            None => "Claude account: not signed in".to_owned(),
        }
    }

    fn build_menu(&self) -> anyhow::Result<Menu> {
        let menu = Menu::new();
        let header = match &self.current {
            Some(name) => format!("Signed in: {name}"),
            None => "Not signed in".to_owned(),
        };
        let mut items: Vec<Box<dyn IsMenuItem>> = vec![
            Box::new(MenuItem::new(header, false, None)),
            Box::new(PredefinedMenuItem::separator()),
        ];
        let accounts: Vec<&PickerRow> = self
            .rows
            .iter()
            .filter(|row| row.account != NEW_SIGN_IN)
            .collect();
        if accounts.is_empty() {
            items.push(Box::new(MenuItem::new(
                "No saved accounts yet",
                false,
                None,
            )));
        }
        for row in accounts {
            let current = row.status == "signed in now";
            let mut text = format!("{}    {}", row.label(), row.last_used);
            if !row.selectable && !current {
                // Only the state; the picker and `profiles` explain what to do.
                let short = row.status.split(':').next().unwrap_or(&row.status);
                text += &format!("    ({short})");
            }
            // Escape `&`, which marks a mnemonic on Windows.
            let text = text.replace('&', "&&");
            items.push(Box::new(CheckMenuItem::with_id(
                format!("{ACCOUNT_PREFIX}{}", row.account),
                text,
                row.selectable,
                current,
                None,
            )));
        }
        items.push(Box::new(PredefinedMenuItem::separator()));
        items.push(Box::new(MenuItem::with_id(
            ID_NEW,
            "Sign in to another account…",
            self.current.is_some(),
            None,
        )));
        items.push(Box::new(MenuItem::with_id(
            ID_PICKER,
            "Choose account…",
            true,
            None,
        )));
        items.push(Box::new(PredefinedMenuItem::separator()));
        items.push(Box::new(MenuItem::with_id(ID_LOG, "Open log", true, None)));
        items.push(Box::new(MenuItem::with_id(
            ID_QUIT,
            "Quit claude-account-sync",
            true,
            None,
        )));
        let refs: Vec<&dyn IsMenuItem> = items.iter().map(|item| item.as_ref()).collect();
        menu.append_items(&refs)?;
        Ok(menu)
    }
}

/// Shows the tray icon and runs the event loop until the user chooses Quit. Returns an error when the icon cannot
/// be created or another process already shows it.
pub fn run(data_dir: PathBuf, state_dir: StateDir, options: TrayOptions) -> anyhow::Result<()> {
    let _tray_lock = state_dir
        .lock_named("tray.lock")
        .context("another claude-account-sync process shows the tray icon")?;
    let icon = load_icon(ICON_PNG)?;

    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        // A menu bar item only: no Dock icon and no menu bar of its own.
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));

    let mut tray: Option<TrayIcon> = None;
    let mut shown: Option<MenuState> = None;
    let mut icon = Some(icon);
    let mut lock = Some(_tray_lock);
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + REFRESH);
        match event {
            // macOS needs the event loop running before the item is created.
            Event::NewEvents(StartCause::Init) => {
                let state = MenuState::read(&data_dir, &state_dir);
                let built = state.build_menu().and_then(|menu| {
                    let icon = icon.take().expect("the icon is used once");
                    #[cfg(target_os = "macos")]
                    let builder = TrayIconBuilder::new().with_icon_templated(icon);
                    #[cfg(not(target_os = "macos"))]
                    let builder = TrayIconBuilder::new().with_icon(icon);
                    builder
                        .with_tooltip(state.tooltip())
                        .with_menu(Box::new(menu))
                        .with_menu_on_left_click(true)
                        .build()
                        .context("cannot create the tray icon")
                });
                match built {
                    Ok(icon) => {
                        tray = Some(icon);
                        shown = Some(state);
                        log::info!("showing the tray icon");
                    }
                    Err(error) => {
                        log::error!("{error:#}");
                        lock.take();
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                let state = MenuState::read(&data_dir, &state_dir);
                if shown.as_ref() != Some(&state)
                    && let Some(tray) = &tray
                    && apply(tray, &state)
                {
                    shown = Some(state);
                }
            }
            Event::UserEvent(UserEvent::Menu(event)) => {
                let id = event.id().0.as_str();
                let dry_run: &[&str] = if options.dry_run { &["--dry-run"] } else { &[] };
                if let Some(account) = id.strip_prefix(ACCOUNT_PREFIX) {
                    spawn_cli(
                        &data_dir,
                        &state_dir,
                        &[&["switch", account, "--gui"], dry_run].concat(),
                    );
                    // The menu toggled the item's check mark on the click. Only a finished switch changes the
                    // signed-in account, so show the actual state again; the refresh shows the new account later.
                    if let Some(tray) = &tray {
                        let state = MenuState::read(&data_dir, &state_dir);
                        if apply(tray, &state) {
                            shown = Some(state);
                        }
                    }
                } else if id == ID_NEW {
                    spawn_cli(
                        &data_dir,
                        &state_dir,
                        &[&["switch", "--new", "--gui"], dry_run].concat(),
                    );
                } else if id == ID_PICKER {
                    spawn_cli(
                        &data_dir,
                        &state_dir,
                        &[&["switch", "--gui"], dry_run].concat(),
                    );
                } else if id == ID_LOG {
                    open_log(&state_dir);
                } else if id == ID_QUIT {
                    log::info!("the user quit from the tray icon");
                    tray.take();
                    lock.take();
                    *control_flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    })
}

fn load_icon(bytes: &[u8]) -> anyhow::Result<Icon> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let mut buffer = vec![0; reader.output_buffer_size().context("icon is too large")?];
    let info = reader.next_frame(&mut buffer)?;
    buffer.truncate(info.buffer_size());
    anyhow::ensure!(
        info.color_type == png::ColorType::Rgba,
        "the tray icon must be an RGBA PNG"
    );
    Ok(Icon::from_rgba(buffer, info.width, info.height)?)
}

/// Replaces the tray's menu and tooltip with `state`. Returns `false` when the menu cannot be built.
fn apply(tray: &TrayIcon, state: &MenuState) -> bool {
    match state.build_menu() {
        Ok(menu) => {
            tray.set_menu(Some(Box::new(menu)));
            let _ = tray.set_tooltip(Some(state.tooltip()));
            true
        }
        Err(error) => {
            log::warn!("cannot rebuild the tray menu: {error:#}");
            false
        }
    }
}

/// Opens the agent's log with the program that the system associates with it. Opens the state folder instead
/// when there is no log, as when the tray runs from `claude-account-sync tray`, which logs to standard error.
fn open_log(state_dir: &StateDir) {
    let log = state_dir.log_file();
    let target = if log.is_file() {
        log
    } else {
        state_dir.root.clone()
    };
    // explorer.exe opens the Documents folder for a path that it cannot parse, such as one with `/`.
    let target = PathBuf::from(
        target
            .to_string_lossy()
            .replace('/', std::path::MAIN_SEPARATOR_STR),
    );
    let program = if cfg!(windows) {
        "explorer.exe"
    } else {
        "open"
    };
    if let Err(error) = std::process::Command::new(program).arg(&target).spawn() {
        log::warn!("cannot open {}: {error}", target.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icon_decodes() {
        load_icon(ICON_PNG).unwrap();
        load_icon(include_bytes!("../assets/png/icon-32.png")).unwrap();
        load_icon(include_bytes!("../assets/png/tray-template-44.png")).unwrap();
    }

    #[test]
    fn tooltip_names_current_account() {
        let state = MenuState {
            current: Some("Work".into()),
            rows: vec![PickerRow {
                account: "aaaaaaaa-0000-0000-0000-000000000001".into(),
                name: "Work".into(),
                email: None,
                last_used: "now".into(),
                status: "signed in now".into(),
                selectable: false,
                removable: false,
            }],
        };
        assert_eq!(state.tooltip(), "Claude account: Work");
    }
}
