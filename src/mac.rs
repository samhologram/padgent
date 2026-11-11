//! A few AppKit calls: menu-bar-only app, show and hide.

#[cfg(target_os = "macos")]
mod imp {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    pub fn set_menu_bar_app() {
        if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
    }

    pub fn show_app() {
        if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);
            app.unhide(None);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
    }

    /// Puts macOS Liquid Glass (or, before macOS 26, a frosted HUD material) *behind* the
    /// app's drawing surface. The surface is an OpenGL view, so anything added inside it
    /// would cover the drawing: the glass goes into its parent, below it.
    pub fn glass_behind(handle: raw_window_handle::RawWindowHandle) -> bool {
        use objc2::{MainThreadOnly, rc::Retained, runtime::AnyClass};
        use objc2_app_kit::{
            NSAutoresizingMaskOptions, NSGlassEffectView, NSGlassEffectViewStyle, NSView, NSVisualEffectBlendingMode,
            NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode,
        };
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let raw_window_handle::RawWindowHandle::AppKit(h) = handle else { return false };
        let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
        let Some(parent) = (unsafe { view.superview() }) else { return false };
        let frame = parent.bounds();
        let resize = NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable;
        let glass: Retained<NSView> = if AnyClass::get(c"NSGlassEffectView").is_some() {
            let g = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
            g.setStyle(NSGlassEffectViewStyle::Regular);
            g.setCornerRadius(16.0);
            Retained::into_super(g)
        } else {
            let v = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
            v.setMaterial(NSVisualEffectMaterial::HUDWindow);
            v.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            v.setState(NSVisualEffectState::Active);
            Retained::into_super(v)
        };
        glass.setAutoresizingMask(resize);
        parent.addSubview_positioned_relativeTo(&glass, NSWindowOrderingMode::Below, Some(view));
        true
    }

    /// Position a logical-size popup below the status item on the screen under the pointer.
    pub fn tray_popup_position(rect: tray_icon::Rect, width: f32, height: f32) -> (f32, f32) {
        use objc2_app_kit::{NSEvent, NSScreen};
        let Some(mtm) = MainThreadMarker::new() else { return (0.0, 24.0) };
        let screens = NSScreen::screens(mtm);
        let Some(primary) = screens.firstObject() else { return (0.0, 24.0) };
        let primary_top = primary.frame().origin.y + primary.frame().size.height;
        let mouse = NSEvent::mouseLocation();
        for screen in screens.iter() {
            let frame = screen.frame();
            if mouse.x >= frame.origin.x && mouse.x < frame.origin.x + frame.size.width
                && mouse.y >= frame.origin.y && mouse.y < frame.origin.y + frame.size.height {
                let scale = screen.backingScaleFactor();
                let visible = screen.visibleFrame();
                let min_x = visible.origin.x as f32;
                let max_x = (visible.origin.x + visible.size.width) as f32 - width;
                let min_y = (primary_top - visible.origin.y - visible.size.height) as f32;
                let max_y = (primary_top - visible.origin.y) as f32 - height;
                let x = ((rect.position.x + f64::from(rect.size.width) / 2.0) / scale) as f32 - width / 2.0;
                let y = ((rect.position.y + f64::from(rect.size.height)) / scale) as f32;
                return (x.clamp(min_x, max_x.max(min_x)), y.clamp(min_y, max_y.max(min_y)));
            }
        }
        (0.0, 24.0)
    }

    // ---- Bringing an app forward ----

    use std::ffi::c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *const c_void;
        static kCGWindowOwnerPID: *const c_void;
        static kCGWindowLayer: *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(a: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(a: *const c_void, i: isize) -> *const c_void;
        fn CFDictionaryGetValue(d: *const c_void, k: *const c_void) -> *const c_void;
        fn CFNumberGetValue(n: *const c_void, kind: isize, out: *mut c_void) -> bool;
        fn CFRelease(cf: *const c_void);
    }
    unsafe extern "C" {
        static _dispatch_main_q: c_void;
        fn dispatch_async_f(queue: *const c_void, ctx: *mut c_void, work: extern "C" fn(*mut c_void));
    }

    type Job = Box<dyn FnOnce() + Send>;

    extern "C" fn run_job(ctx: *mut c_void) {
        let job: Box<Job> = unsafe { Box::from_raw(ctx as *mut Job) };
        job();
    }

    /// Runs `f` on the main thread (works while the window is hidden, unlike a UI frame).
    pub fn on_main(f: Job) {
        let ctx = Box::into_raw(Box::new(f)) as *mut c_void;
        unsafe { dispatch_async_f(&_dispatch_main_q as *const c_void, ctx, run_job) };
    }

    /// Whether `pid` owns a normal on-screen window (CGWindowListCopyWindowInfo with
    /// on-screen-only | exclude-desktop = 0x11, layer 0), as bringForward checks.
    fn has_onscreen_window(pid: i32) -> bool {
        unsafe {
            let list = CGWindowListCopyWindowInfo(0x11, 0);
            if list.is_null() {
                return false;
            }
            let mut found = false;
            for i in 0..CFArrayGetCount(list) {
                let d = CFArrayGetValueAtIndex(list, i);
                let (mut owner, mut layer) = (0i64, -1i64);
                let o = CFDictionaryGetValue(d, kCGWindowOwnerPID);
                let l = CFDictionaryGetValue(d, kCGWindowLayer);
                if !o.is_null() {
                    CFNumberGetValue(o, 4, &mut owner as *mut i64 as *mut c_void);
                }
                if !l.is_null() {
                    CFNumberGetValue(l, 4, &mut layer as *mut i64 as *mut c_void);
                }
                if owner == pid as i64 && layer == 0 {
                    found = true;
                    break;
                }
            }
            CFRelease(list);
            found
        }
    }

    /// HostAppActivator.bringForward (0x100824d8c): yield activation to the app, activate it
    /// from this app with all its windows (else plain activate); when that fails or it has no
    /// window on screen, reopen it from its bundle so it shows one. Call on the main thread.
    pub fn bring_forward(pid: i32) -> bool {
        use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
        let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else { return false };
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).yieldActivationToApplication(&app);
        }
        let me = NSRunningApplication::currentApplication();
        let all = NSApplicationActivationOptions::ActivateAllWindows;
        #[allow(deprecated)]
        let activated = app.activateFromApplication_options(&me, all) || app.activateWithOptions(all);
        crate::mac::log(&format!("bring_forward pid {pid}: activated {activated}"));
        if activated && has_onscreen_window(pid) {
            return true;
        }
        if let Some(path) = app.bundleURL().and_then(|u| u.path()) {
            let _ = std::process::Command::new("/usr/bin/open").arg("-a").arg(path.to_string()).spawn();
        }
        activated
    }

    /// HostAppActivator.activateHostApp (0x1008234e4): from an agent's process, walk up to 64
    /// parents to the first regular (Dock) app — the terminal or editor it runs in.
    pub fn host_app_pid(mut pid: i32) -> Option<i32> {
        use objc2_app_kit::NSRunningApplication;
        for _ in 0..64 {
            if pid < 2 {
                return None;
            }
            if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
                if app.activationPolicy() == NSApplicationActivationPolicy::Regular {
                    return Some(pid);
                }
            }
            let out = std::process::Command::new("/bin/ps").args(["-o", "ppid=", "-p", &pid.to_string()]).output().ok()?;
            pid = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        }
        None
    }

    /// Bundle id of a running app.
    pub fn bundle_id(pid: i32) -> Option<String> {
        use objc2_app_kit::NSRunningApplication;
        NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?.bundleIdentifier().map(|b| b.to_string())
    }

    /// The first running instance of an app.
    pub fn pid_for_bundle(bundle: &str) -> Option<i32> {
        use objc2_app_kit::NSRunningApplication;
        use objc2_foundation::NSString;
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle));
        apps.iter().next().map(|a| a.processIdentifier())
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn glass_behind(_h: raw_window_handle::RawWindowHandle) -> bool {
        false
    }
    pub fn set_menu_bar_app() {}
    pub fn on_main(f: Box<dyn FnOnce() + Send>) {
        f()
    }
    pub fn bring_forward(_pid: i32) -> bool {
        false
    }
    pub fn host_app_pid(_pid: i32) -> Option<i32> {
        None
    }
    pub fn pid_for_bundle(_b: &str) -> Option<i32> {
        None
    }
    pub fn bundle_id(_pid: i32) -> Option<String> {
        None
    }
    pub fn show_app() {}
    pub fn tray_popup_position(rect: tray_icon::Rect, width: f32, _height: f32) -> (f32, f32) {
        (rect.position.x as f32 + rect.size.width as f32 / 2.0 - width / 2.0,
            rect.position.y as f32 + rect.size.height as f32)
    }
}

pub use imp::*;

/// Appends a line to ~/Library/Logs/Padgent.log (kept under 512 KB).
pub fn log(msg: &str) {
    use std::io::Write;
    let Some(home) = dirs::home_dir() else { return };
    let path = home.join("Library/Logs/Padgent.log");
    if std::fs::metadata(&path).map(|m| m.len() > 512 * 1024).unwrap_or(false) {
        let _ = std::fs::write(&path, b"");
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "{t} {msg}");
    }
}
