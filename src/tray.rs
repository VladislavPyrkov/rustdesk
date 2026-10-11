use crate::client::translate;
#[cfg(windows)]
use crate::ipc::Data;
#[cfg(windows)]
use hbb_common::tokio;
use hbb_common::{allow_err, log};
use base::config::keys;
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use std::time::Duration;

pub fn start_tray() {
    if crate::ui_interface::get_builtin_option(keys::OPTION_HIDE_TRAY) == "Y" {
        #[cfg(not(target_os = "macos"))]
        {
            return;
        }
    }

    #[cfg(target_os = "linux")]
    crate::server::check_zombie();

    allow_err!(make_tray());
}

fn make_tray() -> hbb_common::ResultType<()> {
    // https://github.com/tauri-apps/tray-icon/blob/dev/examples/tao.rs
    use hbb_common::anyhow::Context;
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem},
        TrayIcon, TrayIconBuilder, TrayIconEvent as TrayEvent,
    };

    // Duplicated tray icons kept piling up through the blind spots of
    // `check_process("--tray", ..)`. https://github.com/rustdesk/rustdesk/issues/15689
    #[cfg(windows)]
    if !crate::platform::windows::try_lock_tray_single_instance() {
        log::info!("Another tray process is already running in this session, exit");
        return Ok(());
    }

    let icon;
    #[cfg(target_os = "macos")]
    {
        icon = include_bytes!("../res/mac-tray-dark-x2.png"); // use as template, so color is not important
    }
    #[cfg(not(target_os = "macos"))]
    {
        icon = include_bytes!("../res/tray-icon.ico");
    }

    let (icon_rgba, icon_width, icon_height) = {
        let image = load_icon_from_asset()
            .unwrap_or(image::load_from_memory(icon).context("Failed to open icon path")?)
            .into_rgba8();
        let (width, height) = image.dimensions();
        let rgba = image.into_raw();
        (rgba, width, height)
    };
    let icon = tray_icon::Icon::from_rgba(icon_rgba, icon_width, icon_height)
        .context("Failed to open icon")?;
    // ПРО.СТО Remote: theme-aware tray icons (dark/light taskbar, idle/active).
    #[cfg(windows)]
    let mut tray_state = (taskbar_is_light(), false);
    #[cfg(windows)]
    let icon = load_tray_icon(tray_state.0, tray_state.1).unwrap_or(icon);
    #[cfg(windows)]
    let mut last_theme_check = std::time::Instant::now();

    let mut event_loop = EventLoopBuilder::new().build();

    let tray_menu = Menu::new();
    let hide_stop_service = crate::ui_interface::get_builtin_option(
        keys::OPTION_HIDE_STOP_SERVICE,
    ) == "Y";
    // The tray icon is only shown when the service is running, so we don't need to check
    // the `stop-service` option here.
    let quit_i = if !hide_stop_service {
        Some(MenuItem::new(translate("Stop service".to_owned()), true, None))
    } else {
        None
    };
    let open_i = MenuItem::new(translate("Open".to_owned()), true, None);
    if let Some(quit_i) = &quit_i {
        tray_menu.append_items(&[&open_i, quit_i]).ok();
    } else {
        tray_menu.append_items(&[&open_i]).ok();
    }
    let tooltip = |count: usize| {
        if count == 0 {
            format!(
                "{} {}",
                crate::get_app_name(),
                translate("Service is running".to_owned()),
            )
        } else {
            format!(
                "{} - {}\n{}",
                crate::get_app_name(),
                translate("Ready".to_owned()),
                translate("{".to_string() + &format!("{count}") + "} sessions"),
            )
        }
    };
    let mut _tray_icon: Arc<Mutex<Option<TrayIcon>>> = Default::default();

    let menu_channel = MenuEvent::receiver();
    let tray_channel = TrayEvent::receiver();
    #[cfg(windows)]
    let (ipc_sender, ipc_receiver) = std::sync::mpsc::channel::<Data>();

    let open_func = move || {
        if cfg!(not(feature = "flutter")) {
            crate::run_me::<&str>(vec![]).ok();
            return;
        }
        #[cfg(target_os = "macos")]
        crate::platform::macos::handle_application_should_open_untitled_file();
        #[cfg(target_os = "windows")]
        {
            // Do not use "start uni link" way, it may not work on some Windows, and pop out error
            // dialog, I found on one user's desktop, but no idea why, Windows is shit.
            // Use `run_me` instead.
            // `allow_multiple_instances` in `flutter/windows/runner/main.cpp` allows only one instance without args.
            crate::run_me::<&str>(vec![]).ok();
        }
        #[cfg(target_os = "linux")]
        {
            // Do not use "xdg-open", it won't read the config.
            if crate::dbus::invoke_new_connection(crate::get_uri_prefix()).is_err() {
                if let Ok(task) = crate::run_me::<&str>(vec![]) {
                    crate::server::CHILD_PROCESS.lock().unwrap().push(task);
                }
            }
        }
    };

    #[cfg(windows)]
    std::thread::spawn(move || {
        start_query_session_count(ipc_sender.clone());
    });
    #[cfg(windows)]
    let mut last_click = std::time::Instant::now();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::EventLoopExtMacOS;
        event_loop.set_activation_policy(tao::platform::macos::ActivationPolicy::Accessory);
    }
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(100),
        );

        if let tao::event::Event::NewEvents(tao::event::StartCause::Init) = event {
            // for fixing https://github.com/rustdesk/rustdesk/discussions/10210#discussioncomment-14600745
            // so we start tray, but not to show it
            if crate::ui_interface::get_builtin_option(keys::OPTION_HIDE_TRAY) == "Y" {
                return;
            }
            // We create the icon once the event loop is actually running
            // to prevent issues like https://github.com/tauri-apps/tray-icon/issues/90
            let mut builder = TrayIconBuilder::new()
                .with_id(crate::get_app_name().to_lowercase())
                .with_menu(Box::new(tray_menu.clone()))
                .with_tooltip(tooltip(0))
                .with_icon(icon.clone());
            #[cfg(target_os = "macos")]
            {
                builder = builder.with_icon_as_template(true);
            }
            #[cfg(target_os = "windows")]
            {
                // Required since tray-icon 0.17
                // Fixes #15215, #15222, #15410
                builder = builder.with_menu_on_left_click(false);
            }
            let tray = builder.build();
            match tray {
                Ok(tray) => _tray_icon = Arc::new(Mutex::new(Some(tray))),
                Err(err) => {
                    log::error!("Failed to create tray icon: {}", err);
                }
            };

            // We have to request a redraw here to have the icon actually show up.
            // Tao only exposes a redraw method on the Window so we use core-foundation directly.
            #[cfg(target_os = "macos")]
            unsafe {
                use core_foundation::runloop::{CFRunLoopGetMain, CFRunLoopWakeUp};

                let rl = CFRunLoopGetMain();
                CFRunLoopWakeUp(rl);
            }
        }

        if let Ok(event) = menu_channel.try_recv() {
            if let Some(quit_i) = &quit_i {
                if event.id == quit_i.id() {
                    /* failed in windows, seems no permission to check system process
                    if !crate::check_process("--server", false) {
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                    */
                    // Remove the icon first: on success `uninstall_service()` ends
                    // this process with `std::process::exit`, which skips the
                    // destructor that would remove it, leaving a ghost icon behind.
                    #[cfg(windows)]
                    let _ = _tray_icon
                        .lock()
                        .unwrap()
                        .as_mut()
                        .map(|t| t.set_visible(false));
                    if !crate::platform::uninstall_service(false, false) {
                        *control_flow = ControlFlow::Exit;
                    }
                    // Still alive, so stopping the service failed or was cancelled
                    // in the UAC prompt. Show the icon again.
                    #[cfg(windows)]
                    let _ = _tray_icon
                        .lock()
                        .unwrap()
                        .as_mut()
                        .map(|t| t.set_visible(true));
                } else if event.id == open_i.id() {
                    open_func();
                }
            } else if event.id == open_i.id() {
                open_func();
            }
        }

        if let Ok(_event) = tray_channel.try_recv() {
            #[cfg(target_os = "windows")]
            match _event {
                TrayEvent::Click {
                    button,
                    button_state,
                    ..
                } => {
                    if button == tray_icon::MouseButton::Left
                        && button_state == tray_icon::MouseButtonState::Up
                    {
                        if last_click.elapsed() < std::time::Duration::from_secs(1) {
                            return;
                        }
                        open_func();
                        last_click = std::time::Instant::now();
                    }
                }
                _ => {}
            }
        }

        #[cfg(windows)]
        if last_theme_check.elapsed() > std::time::Duration::from_secs(3) {
            last_theme_check = std::time::Instant::now();
            let light = taskbar_is_light();
            if light != tray_state.0 {
                tray_state.0 = light;
                if let Some(new_icon) = load_tray_icon(light, tray_state.1) {
                    _tray_icon.lock().unwrap().as_mut().map(|t| {
                        let _ = t.set_icon(Some(new_icon));
                    });
                }
            }
        }

        #[cfg(windows)]
        if let Ok(data) = ipc_receiver.try_recv() {
            match data {
                Data::ControlledSessionCount(count) => {
                    _tray_icon
                        .lock()
                        .unwrap()
                        .as_mut()
                        .map(|t| t.set_tooltip(Some(tooltip(count))));
                    let active = count > 0;
                    if tray_state.1 != active {
                        tray_state.1 = active;
                        if let Some(new_icon) = load_tray_icon(tray_state.0, active) {
                            _tray_icon.lock().unwrap().as_mut().map(|t| {
                                let _ = t.set_icon(Some(new_icon));
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    });
}

#[cfg(windows)]
fn taskbar_is_light() -> bool {
    use winreg::{enums::*, RegKey};
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("SystemUsesLightTheme"))
        .map(|v| v == 1)
        .unwrap_or(false)
}

#[cfg(windows)]
fn tray_icon_frame(theme: &str, state: &str, size: u32) -> &'static [u8] {
    match (theme, state, size) {
        ("dark", "idle", 16) => include_bytes!("../res/tray/dark-idle-16.png"),
        ("dark", "idle", 20) => include_bytes!("../res/tray/dark-idle-20.png"),
        ("dark", "idle", 24) => include_bytes!("../res/tray/dark-idle-24.png"),
        ("dark", "idle", 32) => include_bytes!("../res/tray/dark-idle-32.png"),
        ("dark", "active", 16) => include_bytes!("../res/tray/dark-active-16.png"),
        ("dark", "active", 20) => include_bytes!("../res/tray/dark-active-20.png"),
        ("dark", "active", 24) => include_bytes!("../res/tray/dark-active-24.png"),
        ("dark", "active", 32) => include_bytes!("../res/tray/dark-active-32.png"),
        ("light", "idle", 16) => include_bytes!("../res/tray/light-idle-16.png"),
        ("light", "idle", 20) => include_bytes!("../res/tray/light-idle-20.png"),
        ("light", "idle", 24) => include_bytes!("../res/tray/light-idle-24.png"),
        ("light", "idle", 32) => include_bytes!("../res/tray/light-idle-32.png"),
        ("light", "active", 16) => include_bytes!("../res/tray/light-active-16.png"),
        ("light", "active", 20) => include_bytes!("../res/tray/light-active-20.png"),
        ("light", "active", 24) => include_bytes!("../res/tray/light-active-24.png"),
        ("light", "active", 32) => include_bytes!("../res/tray/light-active-32.png"),
        _ => include_bytes!("../res/tray/dark-idle-32.png"),
    }
}

#[cfg(windows)]
fn load_tray_icon(light: bool, active: bool) -> Option<tray_icon::Icon> {
    let px = unsafe { winapi::um::winuser::GetSystemMetrics(winapi::um::winuser::SM_CXSMICON) };
    let size = if px <= 16 {
        16
    } else if px <= 20 {
        20
    } else if px <= 24 {
        24
    } else {
        32
    };
    let theme = if light { "light" } else { "dark" };
    let state = if active { "active" } else { "idle" };
    let image = image::load_from_memory(tray_icon_frame(theme, state, size))
        .ok()?
        .into_rgba8();
    let (w, h) = image.dimensions();
    tray_icon::Icon::from_rgba(image.into_raw(), w, h).ok()
}

#[cfg(windows)]
#[tokio::main(flavor = "current_thread")]
async fn start_query_session_count(sender: std::sync::mpsc::Sender<Data>) {
    let mut last_count = 0;
    loop {
        if let Ok(mut c) = crate::ipc::connect(1000, "").await {
            let mut timer = crate::rustdesk_interval(tokio::time::interval(Duration::from_secs(1)));
            loop {
                tokio::select! {
                    res = c.next() => {
                        match res {
                            Err(err) => {
                                log::error!("ipc connection closed: {}", err);
                                break;
                            }

                            Ok(Some(Data::ControlledSessionCount(count))) => {
                                if count != last_count {
                                    last_count = count;
                                    sender.send(Data::ControlledSessionCount(count)).ok();
                                }
                            }
                            _ => {}
                        }
                    }

                    _ = timer.tick() => {
                        c.send(&Data::ControlledSessionCount(0)).await.ok();
                    }
                }
            }
        }
        hbb_common::sleep(1.).await;
    }
}

fn load_icon_from_asset() -> Option<image::DynamicImage> {
    let Some(path) = std::env::current_exe().map_or(None, |x| x.parent().map(|x| x.to_path_buf()))
    else {
        return None;
    };
    #[cfg(target_os = "macos")]
    let path = path.join("../Frameworks/App.framework/Resources/flutter_assets/assets/icon.png");
    #[cfg(windows)]
    let path = path.join(r"data\flutter_assets\assets\icon.png");
    #[cfg(target_os = "linux")]
    let path = path.join(r"data/flutter_assets/assets/icon.png");
    if path.exists() {
        if let Ok(image) = image::open(path) {
            return Some(image);
        }
    }
    None
}
