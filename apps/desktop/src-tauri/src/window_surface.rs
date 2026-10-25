use serde_json::{json, Value};
use tauri::{window::Color, Theme, WebviewWindow};

#[tauri::command]
pub(super) async fn desktop_window_surface(
    window: WebviewWindow,
    dark: bool,
    reduce_transparency: bool,
    observe: bool,
) -> Result<Value, String> {
    // Tauri 2.9.1 set_effects only acknowledges dispatch, drops apply errors,
    // and does not clear macOS effects. Report completion from the main thread.
    let (sender, mut receiver) = tauri::async_runtime::channel(1);
    let target = window.clone();
    window
        .run_on_main_thread(move || {
            #[cfg(target_os = "macos")]
            macos_observer::synchronize(&target, observe);
            #[cfg(not(target_os = "macos"))]
            let _ = observe;
            let result = synchronize(&target, dark, reduce_transparency);
            let _ = sender.try_send(result);
        })
        .map_err(|error| error.to_string())?;
    receiver
        .recv()
        .await
        .ok_or_else(|| "Window surface callback was released".to_owned())?
}

fn synchronize(
    window: &WebviewWindow,
    dark: bool,
    reduce_transparency: bool,
) -> Result<Value, String> {
    let solid = if dark {
        Color(31, 37, 49, 255)
    } else {
        Color(245, 243, 245, 255)
    };
    window
        .set_theme(Some(if dark { Theme::Dark } else { Theme::Light }))
        .map_err(|error| error.to_string())?;
    window
        .set_background_color(Some(solid))
        .map_err(|error| error.to_string())?;
    let platform = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    };
    let glass = match platform_surface(window, reduce_transparency) {
        Ok(glass) => glass,
        Err(error) => {
            eprintln!("Window material unavailable; using solid surface: {error}");
            false
        }
    };
    if glass {
        if let Err(error) = window.set_background_color(Some(Color(0, 0, 0, 0))) {
            let _ = platform_surface(window, true);
            let _ = window.set_background_color(Some(solid));
            return Err(error.to_string());
        }
    }
    Ok(json!({ "platform": platform, "material": if glass { "glass" } else { "solid" } }))
}

#[cfg(target_os = "macos")]
fn platform_surface(window: &WebviewWindow, reduce_transparency: bool) -> Result<bool, String> {
    use objc2::msg_send;
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSUserInterfaceItemIdentification, NSView,
        NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
        NSVisualEffectView, NSWindow, NSWindowOrderingMode, NSWorkspace,
    };
    use objc2_foundation::{ns_string, MainThreadMarker};

    let mtm = MainThreadMarker::new().ok_or("Window material requires the AppKit main thread")?;
    let pointer = window.ns_window().map_err(|error| error.to_string())?;
    // Tauri owns this NSWindow; borrow it only for this main-thread callback.
    let native = unsafe { (pointer as *const NSWindow).as_ref() }.ok_or("Missing native window")?;
    // The window owns its content view throughout this synchronous callback.
    // Borrow the +0 getter result; do not transfer it into an owning wrapper.
    let content: *mut NSView = unsafe { msg_send![native, contentView] };
    let content = unsafe { content.as_ref() }.ok_or("Missing native content view")?;
    let identifier = ns_string!("com.mint.cat.window-material");
    let reduced = reduce_transparency
        || NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency();
    let existing = content
        .subviews()
        .into_iter()
        .find(|view| view.identifier().as_deref() == Some(identifier));
    if reduced {
        if let Some(view) = existing {
            view.removeFromSuperview();
        }
        return Ok(false);
    }
    if existing.is_none() {
        // Use a whole-window backdrop material, not the denser Sidebar tint.
        // The window's Light/Dark appearance controls the native blur's tint.
        let effect = NSVisualEffectView::initWithFrame(mtm.alloc(), content.bounds());
        effect.setIdentifier(Some(identifier));
        effect.setMaterial(NSVisualEffectMaterial::UnderWindowBackground);
        effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        effect.setState(NSVisualEffectState::Active);
        effect.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview_positioned_relativeTo(&effect, NSWindowOrderingMode::Below, None);
    }
    Ok(true)
}

#[cfg(windows)]
fn platform_surface(window: &WebviewWindow, reduce_transparency: bool) -> Result<bool, String> {
    use windows::Win32::Graphics::Dwm::{
        DwmIsCompositionEnabled, DwmSetWindowAttribute, DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW,
        DWMWA_SYSTEMBACKDROP_TYPE,
    };
    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    // Public system-backdrop Acrylic (Windows 11 22H2+). Older Windows rejects
    // this attribute and remains solid, avoiding legacy Acrylic drag/resize lag.
    unsafe {
        if !DwmIsCompositionEnabled()
            .map_err(|error| error.to_string())?
            .as_bool()
        {
            return Ok(false);
        }
        let backdrop = if reduce_transparency {
            DWMSBT_NONE
        } else {
            DWMSBT_TRANSIENTWINDOW
        };
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const _ as *const std::ffi::c_void,
            std::mem::size_of_val(&backdrop) as u32,
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(!reduce_transparency)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_surface(_window: &WebviewWindow, _reduce_transparency: bool) -> Result<bool, String> {
    Ok(false)
}

#[cfg(target_os = "macos")]
mod macos_observer {
    use block2::RcBlock;
    use objc2::{rc::Retained, runtime::ProtocolObject};
    use objc2_app_kit::{NSWorkspace, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification};
    use objc2_foundation::{NSNotificationCenter, NSObjectProtocol};
    use std::{cell::RefCell, collections::HashMap};
    use tauri::{Emitter, WebviewWindow};

    struct Observer {
        center: Retained<NSNotificationCenter>,
        token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    }

    impl Drop for Observer {
        fn drop(&mut self) {
            // Tokens are created, removed and dropped only on the AppKit thread.
            unsafe {
                self.center.removeObserver((&*self.token).as_ref());
            }
        }
    }

    thread_local! {
        static OBSERVERS: RefCell<HashMap<String, Observer>> = RefCell::new(HashMap::new());
    }

    pub(super) fn synchronize(window: &WebviewWindow, observe: bool) {
        if !observe {
            release(window.label());
            return;
        }
        OBSERVERS.with(|observers| {
            let mut observers = observers.borrow_mut();
            if observers.contains_key(window.label()) {
                return;
            }
            let target = window.clone();
            // The callback only captures a Send+Sync Tauri handle; NSWorkspace
            // may post from any thread. Frontend synchronization returns to main.
            let block = RcBlock::new(move |_| {
                let _ = target.emit("mintcat-window-transparency", ());
            });
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
                    None,
                    None,
                    &block,
                )
            };
            observers.insert(window.label().to_owned(), Observer { center, token });
        });
    }

    pub(super) fn release(label: &str) {
        let observer = OBSERVERS.with(|observers| observers.borrow_mut().remove(label));
        drop(observer);
    }

    pub(super) fn release_all() {
        let observers = OBSERVERS.with(|tracked| {
            tracked
                .borrow_mut()
                .drain()
                .map(|(_, observer)| observer)
                .collect::<Vec<_>>()
        });
        drop(observers);
    }
}

#[cfg(target_os = "macos")]
pub(super) fn prepare_for_destruction(label: &str) {
    macos_observer::release(label);
}

#[cfg(target_os = "macos")]
pub(super) fn prepare_for_termination() {
    // Application-level quit may skip per-window events.
    macos_observer::release_all();
}
