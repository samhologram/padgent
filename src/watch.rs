//! Follows conversations in the ChatGPT and Claude desktop apps through macOS
//! Accessibility: each open window is a conversation; a visible "Stop" button means a
//! reply is being written; the last long text is its last message. Read-only.

use crate::model::{Core, Incoming};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Opens a followed app on one conversation: raise the window with that title, or press the
/// sidebar entry with that title (the way legacy opens an exact task "through Accessibility").
pub fn focus_conversation(bundle: &str, title: &str, electron: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        if !ax::trusted() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
        fn find(el: ax::CFTypeRef, title: &str, depth: usize, budget: &mut usize) -> bool {
            if depth > 40 || *budget == 0 {
                return false;
            }
            *budget -= 1;
            let label = [ax::str_attr(el, "AXTitle"), ax::str_attr(el, "AXValue"), ax::str_attr(el, "AXDescription")]
                .into_iter()
                .flatten()
                .any(|s| s.trim() == title);
            if label {
                // Press it, or the nearest pressable parent (a row or link).
                if ax::press(el, "AXPress") {
                    return true;
                }
                let mut cur = ax::attr(el, "AXParent");
                for _ in 0..4 {
                    let Some(p) = cur else { break };
                    if ax::press(p.0, "AXPress") {
                        return true;
                    }
                    if ax::set_true(p.0, "AXSelected") {
                        return true;
                    }
                    cur = ax::attr(p.0, "AXParent");
                }
            }
            let (_keep, kids) = ax::list(el, "AXChildren");
            kids.into_iter().any(|k| find(k, title, depth + 1, budget))
        }
        for pid in pids(bundle) {
            let app = ax::app(pid, electron);
            let (_keep, wins) = ax::list(app.0, "AXWindows");
            for w in &wins {
                let t = ax::str_attr(*w, "AXTitle").unwrap_or_default();
                if !title.is_empty() && t.contains(title) {
                    ax::press(*w, "AXRaise");
                    ax::set_true(*w, "AXMain");
                    return true;
                }
            }
            for w in &wins {
                let mut budget = 5000;
                if find(*w, title, 0, &mut budget) {
                    ax::press(*w, "AXRaise");
                    ax::set_true(*w, "AXMain");
                    return true;
                }
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (bundle, title, electron);
    false
}

/// Running apps with a bundle id, for picking which app an agent follows.
pub fn running_apps() -> Vec<(String, String)> {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<(Instant, Vec<(String, String)>)>> = Mutex::new(None);
    if let Ok(mut c) = CACHE.lock() {
        if let Some((at, v)) = c.as_ref() {
            if at.elapsed() < Duration::from_secs(2) {
                return v.clone();
            }
        }
        let v = running_apps_now();
        *c = Some((Instant::now(), v.clone()));
        return v;
    }
    running_apps_now()
}

fn running_apps_now() -> Vec<(String, String)> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};
        let ws = NSWorkspace::sharedWorkspace();
        let mut v: Vec<(String, String)> = ws
            .runningApplications()
            .iter()
            .filter(|a| a.activationPolicy() == NSApplicationActivationPolicy::Regular)
            .filter_map(|a| {
                let b = a.bundleIdentifier()?.to_string();
                let n = a.localizedName().map(|n| n.to_string()).unwrap_or_else(|| b.clone());
                Some((n, b))
            })
            .collect();
        v.sort();
        v.dedup();
        v
    }
    #[cfg(not(target_os = "macos"))]
    Vec::new()
}

pub fn trusted() -> bool {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    if let Ok(mut c) = CACHE.lock() {
        if let Some((at, v)) = *c {
            if at.elapsed() < Duration::from_secs(2) {
                return v;
            }
        }
        let v = trusted_now();
        *c = Some((Instant::now(), v));
        return v;
    }
    trusted_now()
}

fn trusted_now() -> bool {
    #[cfg(target_os = "macos")]
    return ax::trusted();
    #[cfg(not(target_os = "macos"))]
    false
}

/// Opens Privacy & Security › Input Monitoring (needed to read the physical keypad).
pub fn request_input_monitoring() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
        .spawn();
}

/// Name and bundle id of an app on disk (reads its Info.plist with PlistBuddy).
pub fn app_info(path: &std::path::Path) -> Option<(String, String)> {
    let plist = path.join("Contents/Info.plist");
    let read = |key: &str| {
        std::process::Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", &format!("Print :{key}")])
            .arg(&plist)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let bundle = read("CFBundleIdentifier")?;
    let name = read("CFBundleDisplayName")
        .or_else(|| read("CFBundleName"))
        .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| bundle.clone()));
    Some((name, bundle))
}

/// Bundle id AgentPad registers with macOS (Info.plist written by bundle.sh).
pub const OWN_BUNDLE_ID: &str = "com.agentpad.app";

/// The running copy's code signature (CDHash), which macOS ties the Accessibility grant to.
fn running_requirement() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let out = std::process::Command::new("/usr/bin/codesign").args(["-dvvv"]).arg(&exe).output().ok()?;
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    text.lines().find_map(|l| l.strip_prefix("CDHash=")).map(|s| s.trim().to_string())
}

/// AppController.recoverStaleAccessibilityIfNeeded (0x1000b9350): when Accessibility isn't
/// trusted, an app is being followed, and this is a new code signature, reset AgentPad's
/// own entry once with `tccutil reset Accessibility <bundle id>`
/// (AccessibilityRecovery.resetOwnEntry 0x100003784), then ask macOS to prompt. Returns
/// Some(requirement) when it ran, with whether the reset worked.
pub fn recover_stale_accessibility(feature_in_use: bool, last: &str) -> Option<(String, bool)> {
    if trusted() || !feature_in_use {
        return None;
    }
    let req = running_requirement()?;
    if req == last {
        return None;
    }
    let ok = std::process::Command::new("/usr/bin/tccutil")
        .args(["reset", "Accessibility", OWN_BUNDLE_ID])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if ok {
        #[cfg(target_os = "macos")]
        unsafe {
            ax::prompt();
        }
    }
    Some((req, ok))
}

pub fn request_access() {
    #[cfg(target_os = "macos")]
    unsafe {
        ax::prompt();
    }
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}

#[cfg(target_os = "macos")]
mod ax {
    use std::ffi::c_void;
    pub type CFTypeRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
        fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFTypeRef, value: *mut CFTypeRef) -> i32;
        fn AXUIElementSetAttributeValue(el: CFTypeRef, attr: CFTypeRef, value: CFTypeRef) -> i32;
        fn AXUIElementSetMessagingTimeout(el: CFTypeRef, t: f32) -> i32;
        fn AXUIElementPerformAction(el: CFTypeRef, action: CFTypeRef) -> i32;
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> bool;
        static kAXTrustedCheckOptionPrompt: CFTypeRef;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(a: CFTypeRef, b: *const u8, n: isize, enc: u32, ext: bool) -> CFTypeRef;
        fn CFRelease(cf: CFTypeRef);
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFArrayGetTypeID() -> usize;
        fn CFArrayGetCount(a: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
        fn CFStringGetLength(s: CFTypeRef) -> isize;
        fn CFStringGetMaximumSizeForEncoding(len: isize, enc: u32) -> isize;
        fn CFStringGetCString(s: CFTypeRef, buf: *mut u8, size: isize, enc: u32) -> bool;
        fn CFDictionaryCreate(a: CFTypeRef, k: *const CFTypeRef, v: *const CFTypeRef, n: isize, kc: CFTypeRef, vc: CFTypeRef) -> CFTypeRef;
        static kCFBooleanTrue: CFTypeRef;
    }

    pub struct Cf(pub CFTypeRef);
    impl Drop for Cf {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) }
            }
        }
    }

    pub fn cfstr(s: &str) -> Cf {
        Cf(unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, UTF8, false) })
    }

    pub fn trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    pub unsafe fn prompt() {
        unsafe {
            let k = [kAXTrustedCheckOptionPrompt];
            let v = [kCFBooleanTrue];
            let d = CFDictionaryCreate(std::ptr::null(), k.as_ptr(), v.as_ptr(), 1, std::ptr::null(), std::ptr::null());
            AXIsProcessTrustedWithOptions(d);
            if !d.is_null() {
                CFRelease(d);
            }
        }
    }

    pub fn app(pid: i32, electron: bool) -> Cf {
        let el = Cf(unsafe { AXUIElementCreateApplication(pid) });
        unsafe { AXUIElementSetMessagingTimeout(el.0, 0.4) };
        if electron {
            let k = cfstr("AXManualAccessibility");
            unsafe { AXUIElementSetAttributeValue(el.0, k.0, kCFBooleanTrue) };
        }
        el
    }

    pub fn attr(el: CFTypeRef, name: &str) -> Option<Cf> {
        let k = cfstr(name);
        let mut out: CFTypeRef = std::ptr::null();
        let err = unsafe { AXUIElementCopyAttributeValue(el, k.0, &mut out) };
        if err == 0 && !out.is_null() { Some(Cf(out)) } else { None }
    }

    pub fn string(cf: &Cf) -> Option<String> {
        unsafe {
            if CFGetTypeID(cf.0) != CFStringGetTypeID() {
                return None;
            }
            let len = CFStringGetLength(cf.0);
            let size = CFStringGetMaximumSizeForEncoding(len, UTF8) + 1;
            let mut buf = vec![0u8; size.max(1) as usize];
            if !CFStringGetCString(cf.0, buf.as_mut_ptr(), size, UTF8) {
                return None;
            }
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            Some(String::from_utf8_lossy(&buf[..end]).to_string())
        }
    }

    pub fn press(el: CFTypeRef, action: &str) -> bool {
        let a = cfstr(action);
        unsafe { AXUIElementPerformAction(el, a.0) == 0 }
    }

    pub fn set_true(el: CFTypeRef, name: &str) -> bool {
        let k = cfstr(name);
        unsafe { AXUIElementSetAttributeValue(el, k.0, kCFBooleanTrue) == 0 }
    }

    pub fn str_attr(el: CFTypeRef, name: &str) -> Option<String> {
        attr(el, name).and_then(|c| string(&c))
    }

    /// Children of an array attribute; the array stays alive while they're used.
    pub fn list(el: CFTypeRef, name: &str) -> (Option<Cf>, Vec<CFTypeRef>) {
        let Some(arr) = attr(el, name) else { return (None, Vec::new()) };
        unsafe {
            if CFGetTypeID(arr.0) != CFArrayGetTypeID() {
                return (Some(arr), Vec::new());
            }
            let n = CFArrayGetCount(arr.0);
            let items = (0..n).map(|i| CFArrayGetValueAtIndex(arr.0, i)).collect();
            (Some(arr), items)
        }
    }
}

#[cfg(target_os = "macos")]
fn pids(bundle: &str) -> Vec<i32> {
    use objc2_app_kit::NSRunningApplication;
    use objc2_foundation::NSString;
    let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle));
    apps.iter().map(|a| a.processIdentifier()).collect()
}

struct Window {
    title: String,
    generating: bool,
    last_text: String,
    /// Conversation titles listed in the app's sidebar (most recent first).
    recent: Vec<String>,
}

/// Sidebar entries that are app controls, not conversations.
fn is_control_label(s: &str) -> bool {
    let l = s.trim().to_lowercase();
    const WORDS: [&str; 24] = [
        "new chat", "new conversation", "search", "settings", "projects", "library", "explore", "gpts", "chats",
        "recents", "starred", "today", "yesterday", "previous 7 days", "previous 30 days", "upgrade", "help",
        "new task", "tasks", "code", "home", "archive", "sora", "codex",
    ];
    l.len() < 3 || l.len() > 90 || WORDS.contains(&l.as_str()) || l.starts_with("previous ")
}

#[cfg(target_os = "macos")]
fn scan(el: ax::CFTypeRef, depth: usize, budget: &mut usize, w: &mut Window, in_list: bool) {
    if depth > 40 || *budget == 0 {
        return;
    }
    *budget -= 1;
    let role = ax::str_attr(el, "AXRole").unwrap_or_default();
    let list_here = in_list || matches!(role.as_str(), "AXOutline" | "AXList" | "AXTable");
    if in_list && matches!(role.as_str(), "AXStaticText" | "AXLink" | "AXButton" | "AXCell") && w.recent.len() < 12 {
        let label = ax::str_attr(el, "AXTitle")
            .filter(|s| !s.trim().is_empty())
            .or_else(|| ax::str_attr(el, "AXValue"))
            .or_else(|| ax::str_attr(el, "AXDescription"))
            .unwrap_or_default();
        let label = label.trim().to_string();
        if !is_control_label(&label) && !w.recent.contains(&label) {
            w.recent.push(label);
        }
    }
    if role == "AXButton" {
        let label = [ax::str_attr(el, "AXDescription"), ax::str_attr(el, "AXTitle"), ax::str_attr(el, "AXHelp")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if label.contains("stop") && (label.contains("generat") || label.contains("respon") || label.contains("stream") || label.trim() == "stop") {
            w.generating = true;
        }
    } else if role == "AXStaticText" || role == "AXTextArea" {
        if let Some(v) = ax::str_attr(el, "AXValue") {
            let v = v.trim();
            if v.chars().count() >= 40 && role == "AXStaticText" {
                w.last_text = v.chars().take(400).collect();
            }
        }
    }
    let (_keep, kids) = ax::list(el, "AXChildren");
    for k in kids {
        scan(k, depth + 1, budget, w, list_here);
    }
}

#[cfg(target_os = "macos")]
fn windows(pid: i32, electron: bool) -> Vec<Window> {
    let app = ax::app(pid, electron);
    let (_keep, wins) = ax::list(app.0, "AXWindows");
    let mut out = Vec::new();
    for w in wins.into_iter().take(6) {
        let title = ax::str_attr(w, "AXTitle").unwrap_or_default();
        let mut win = Window { title, generating: false, last_text: String::new(), recent: Vec::new() };
        let mut budget = 3500;
        scan(w, 0, &mut budget, &mut win, false);
        out.push(win);
    }
    out
}

pub fn start(core: Arc<Mutex<Core>>, wake: Arc<dyn Fn() + Send + Sync>) {
    #[cfg(target_os = "macos")]
    std::thread::Builder::new()
        .name("app-watch".into())
        .spawn(move || {
            // (agent, window) -> (generating, last text)
            let mut last: HashMap<(String, String), (bool, String)> = HashMap::new();
            loop {
                std::thread::sleep(Duration::from_millis(2500));
                if !ax::trusted() {
                    continue;
                }
                let apps = core.lock().map(|c| c.watched_apps()).unwrap_or_default();
                for (agent, bundle, electron) in apps {
                    let agent = agent.as_str();
                    for pid in pids(&bundle) {
                        for w in windows(pid, electron) {
                            let raw = w.title.trim();
                            let title = raw
                                .trim_end_matches(" - ChatGPT")
                                .trim_end_matches(" - Claude")
                                .trim();
                            let app_named = core.lock().ok().and_then(|c| c.agent(agent).map(|a| a.name == title || a.app_name == title)).unwrap_or(false);
                            if title.is_empty() || app_named {
                                if !w.generating && w.last_text.is_empty() {
                                    continue;
                                }
                            }
                            let id = if title.is_empty() { "current".to_string() } else { title.to_string() };
                            // A Claude Code conversation in the Claude app already has its own key.
                            let is_code = core
                                .lock()
                                .map(|c| c.sessions.iter().any(|s| s.agent == "claude-code" && s.title == id))
                                .unwrap_or(false);
                            if is_code {
                                continue;
                            }
                            let key = (agent.to_string(), id.clone());
                            let prev = last.get(&key).cloned();
                            let state = if w.generating {
                                "streaming"
                            } else if prev.as_ref().map(|p| p.0).unwrap_or(false) {
                                "done"
                            } else if prev.is_none() {
                                "idle"
                            } else {
                                continue_state(&prev)
                            };
                            // Only conversations that are (or were just) writing are sessions.
                            if prev.is_none() && !w.generating {
                                continue;
                            }
                            let changed = prev.as_ref() != Some(&(w.generating, w.last_text.clone()));
                            if !changed {
                                continue;
                            }
                            last.insert(key, (w.generating, w.last_text.clone()));
                            let fallback = core.lock().ok().and_then(|c| c.agent(agent).map(|a| a.name.clone())).unwrap_or_default();
                            let shown = if title.is_empty() { fallback.as_str() } else { title };
                            if let Ok(mut c) = core.lock() {
                                c.apply(Incoming {
                                    agent: agent.to_string(),
                                    event: None,
                                    body: json!({ "session": id, "state": state, "title": shown, "text": w.last_text, "_app": bundle }),
                                });
                            }
                            wake();
                        }
                    }
                }
            }
        })
        .ok();
    #[cfg(not(target_os = "macos"))]
    let _ = (core, wake);
}

fn continue_state(_prev: &Option<(bool, String)>) -> &'static str {
    "done"
}
