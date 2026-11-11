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
        use objc2::{AnyThread, MainThreadOnly, rc::Retained, runtime::AnyClass};
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

    pub fn hide_app() {
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).hide(None);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn glass_behind(_h: raw_window_handle::RawWindowHandle) -> bool {
        false
    }
    pub fn set_menu_bar_app() {}
    pub fn show_app() {}
    pub fn hide_app() {}
}

pub use imp::*;
