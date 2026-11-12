//! Follows conversations in the ChatGPT and Claude desktop apps through macOS
//! Accessibility: each open window is a conversation; a visible "Stop" button means a
//! reply is being written; the last long text is its last message. Read-only.

use crate::model::{Core, Incoming};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Opens a followed app on one conversation: raise the window with that title, or press the
/// sidebar entry with that title through Accessibility.
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
        fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> bool;
        fn CFArrayGetTypeID() -> usize;
        fn CFURLGetTypeID() -> usize;
        fn CFURLGetString(url: CFTypeRef) -> CFTypeRef;
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

    pub fn url_attr(el: CFTypeRef, name: &str) -> Option<String> {
        let value = attr(el, name)?;
        unsafe {
            if CFGetTypeID(value.0) == CFURLGetTypeID() {
                let url = CFURLGetString(value.0);
                if url.is_null() { return None; }
                // CFURLGetString returns a borrowed reference; retain it for Cf.
                CFRetain(url);
                string(&Cf(url))
            } else {
                string(&value)
            }
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

    pub fn number_attr(el: CFTypeRef, name: &str) -> Option<f64> {
        let number = attr(el, name)?;
        unsafe {
            if CFGetTypeID(number.0) != CFNumberGetTypeID() { return None; }
            let mut value = 0.0;
            if CFNumberGetValue(number.0, 6, &mut value as *mut f64 as *mut c_void) { Some(value) } else { None }
        }
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
    claude_webarea: Option<(String, String)>,
    usage_labels: Vec<String>,
    usage_levels: Vec<(String, f64, String)>,
    usage_reset: String,
    generating: bool,
    running_titles: Vec<String>,
    last_text: String,
    reply: ReplyObservation,
    /// Conversation titles listed in the app's sidebar (most recent first).
    recent: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum MessageActor { #[default] Unknown, Assistant, User }

#[derive(Default)]
struct ReplyObservation { actor: MessageActor, text: String }

impl ReplyObservation {
    fn new_message(&mut self) { self.actor = MessageActor::Unknown; self.text.clear(); }
    fn heading(&mut self, label: &str) {
        let label = label.trim().to_lowercase();
        if label.starts_with("claude responded:") || label.starts_with("chatgpt said:") {
            self.actor = MessageActor::Assistant;
        } else if label.starts_with("you said:") {
            self.actor = MessageActor::User;
        }
    }
    fn text(&mut self, value: &str) {
        if self.actor == MessageActor::Assistant && !value.trim().is_empty() {
            self.text = value.trim().chars().rev().take(400).collect::<String>().chars().rev().collect();
        }
    }
    fn is_question(&self, generating: bool) -> bool {
        !generating && self.actor == MessageActor::Assistant && self.text.trim_end().ends_with('?')
    }
}

fn is_message_wrapper(label: &str) -> bool {
    let label = label.trim();
    (label.starts_with("Message ") && label.contains(" of ")) || label == "Currently streaming message"
}

/// Extract the stable ID only from a Claude standard chat page, never a Code route.
fn claude_chat_id(url: &str) -> Option<String> {
    let path = url.strip_prefix("https://claude.ai/chat/")?;
    let id = path.split(['/', '?', '#']).next()?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') { return None; }
    Some(id.to_string())
}

/// Keep the standard-chat web area when a window also contains host or utility webviews.
fn select_claude_webarea(current: &mut Option<(String, String)>, url: &str, title: &str) {
    if claude_chat_id(url).is_some() {
        *current = Some((url.to_string(), title.to_string()));
    }
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

// A long transcript can exhaust the AX node budget before the composer and its
// Stop button are reached. Keep the latest message and controls discoverable.
fn recent_message_start(label: &str, children: usize) -> usize {
    if label.trim().eq_ignore_ascii_case("chat messages") {
        children.saturating_sub(3)
    } else {
        0
    }
}

fn message_child_start(label: &str, in_messages: bool, children: usize) -> usize {
    if in_messages && children > 3 { children - 3 } else { recent_message_start(label, children) }
}

fn running_title(label: &str) -> Option<&str> {
    label.trim().strip_suffix(" Running").map(str::trim).filter(|s| !s.is_empty())
}

fn current_conversation_running(agent: &str, title: &str, running_titles: &[String]) -> bool {
    agent == "claude-desktop" && !title.is_empty() && running_titles.iter().any(|t| t == title)
}

#[cfg(target_os = "macos")]
fn scan(el: ax::CFTypeRef, depth: usize, budget: &mut usize, w: &mut Window, in_list: bool, in_messages: bool, in_message: bool) {
    if depth > 40 || *budget == 0 {
        return;
    }
    *budget -= 1;
    let role = ax::str_attr(el, "AXRole").unwrap_or_default();
    let label = if role == "AXGroup" || role == "AXHeading" {
        ax::str_attr(el, "AXDescription").filter(|s| !s.is_empty())
            .or_else(|| ax::str_attr(el, "AXTitle")).unwrap_or_default()
    } else { String::new() };
    let message_here = role == "AXGroup" && is_message_wrapper(&label);
    if message_here { w.reply.new_message(); }
    let in_message = in_message || message_here;
    if in_message && role == "AXHeading" { w.reply.heading(&label); }
    // Message action bars contain timestamps and controls, not response text.
    if in_message && role == "AXToolbar" { return; }
    if role == "AXWebArea" {
        let title = ax::str_attr(el, "AXTitle").unwrap_or_default();
        let url = ax::url_attr(el, "AXURL").unwrap_or_default();
        select_claude_webarea(&mut w.claude_webarea, &url, &title);
    }
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
    if matches!(role.as_str(), "AXButton" | "AXPopUpButton") {
        for label in [ax::str_attr(el, "AXDescription"), ax::str_attr(el, "AXTitle")].into_iter().flatten() {
            if let Some(title) = running_title(&label) { w.running_titles.push(title.to_string()); }
        }
        for label in [ax::str_attr(el, "AXDescription"), ax::str_attr(el, "AXTitle")].into_iter().flatten() {
            if label.trim().starts_with("Usage:") { w.usage_labels.push(label); }
        }
        let label = [ax::str_attr(el, "AXDescription"), ax::str_attr(el, "AXTitle"), ax::str_attr(el, "AXHelp")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if label.contains("stop") && (label.contains("generat") || label.contains("respon") || label.contains("stream") || label.trim() == "stop") {
            w.generating = true;
        }
    } else if role == "AXLevelIndicator" {
        let title = ax::str_attr(el, "AXTitle").unwrap_or_default();
        if let Some(value) = ax::number_attr(el, "AXValue") {
            w.usage_levels.push((title, value, w.usage_reset.clone()));
        }
    } else if role == "AXStaticText" || role == "AXTextArea" {
        if let Some(v) = ax::str_attr(el, "AXValue") {
            let v = v.trim();
            if v.eq_ignore_ascii_case("5-hour limit") || v.to_lowercase().starts_with("weekly · ") { w.usage_reset.clear(); }
            if v.starts_with("Resets ") { w.usage_reset = v.to_string(); }
            if v.eq_ignore_ascii_case("Claude is responding") { w.generating = true; }
            if in_message && role == "AXStaticText" { w.reply.text(v); }
            if v.chars().count() >= 40 && role == "AXStaticText" {
                w.last_text = v.chars().take(400).collect();
            }
        }
    }
    let (_keep, kids) = ax::list(el, "AXChildren");
    let messages_here = label.trim().eq_ignore_ascii_case("chat messages");
    let start = message_child_start(&label, in_messages, kids.len());
    for k in kids.into_iter().skip(start) {
        scan(k, depth + 1, budget, w, list_here, messages_here, in_message);
    }
}

#[cfg(target_os = "macos")]
fn windows(pid: i32, electron: bool) -> Vec<Window> {
    let app = ax::app(pid, electron);
    let (_keep, wins) = ax::list(app.0, "AXWindows");
    let mut out = Vec::new();
    for w in wins.into_iter().take(6) {
        let title = ax::str_attr(w, "AXTitle").unwrap_or_default();
        let mut win = Window { title, claude_webarea: None, usage_labels: Vec::new(), usage_levels: Vec::new(), usage_reset: String::new(), generating: false, running_titles: Vec::new(), last_text: String::new(), reply: ReplyObservation::default(), recent: Vec::new() };
        let mut budget = 3500;
        scan(w, 0, &mut budget, &mut win, false, false, false);
        out.push(win);
    }
    out
}

pub fn start(core: Arc<Mutex<Core>>, wake: Arc<dyn Fn() + Send + Sync>) {
    #[cfg(target_os = "macos")]
    std::thread::Builder::new()
        .name("app-watch".into())
        .spawn(move || {
            // (agent, conversation) -> (generating, last text, title)
            let mut last: HashMap<(String, String), (bool, bool, String, String)> = HashMap::new();
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
                            if agent == "claude-desktop" {
                                for (window, used, reset) in &w.usage_levels {
                                    if let Some(observation) = crate::quota::parse_claude_window(window, *used, reset, &crate::quota::observed_now()) {
                                        if let Ok(mut c) = core.lock() { crate::quota::observe(&mut c, observation); }
                                        wake();
                                    }
                                }
                                for label in &w.usage_labels {
                                    if let Some(observation) = crate::quota::parse_claude_usage(label, &crate::quota::observed_now()) {
                                        if let Ok(mut c) = core.lock() { crate::quota::observe(&mut c, observation); }
                                        wake();
                                    }
                                }
                            }
                            // Claude Desktop also hosts Code. Only a verified chat URL is a
                            // standard conversation; never infer it from the generic window title.
                            let claude_chat = if agent == "claude-desktop" {
                                match w.claude_webarea.as_ref().and_then(|(url, _)| claude_chat_id(url)) {
                                    Some(id) => Some(id),
                                    None => continue,
                                }
                            } else { None };
                            let raw = if claude_chat.is_some() { w.claude_webarea.as_ref().map(|(_, title)| title.trim()).unwrap_or_default() } else { w.title.trim() };
                            let title = raw
                                .trim_end_matches(" - ChatGPT")
                                .trim_end_matches(" - Claude")
                                .trim();
                            let generating = w.generating || current_conversation_running(agent, title, &w.running_titles);
                            let question = w.reply.is_question(generating);
                            let text = if w.reply.actor == MessageActor::Assistant && !w.reply.text.is_empty() { &w.reply.text } else { &w.last_text };
                            let app_named = core.lock().ok().and_then(|c| c.agent(agent).map(|a| a.name == title || a.app_name == title)).unwrap_or(false);
                            if title.is_empty() || app_named {
                                if !generating && w.last_text.is_empty() {
                                    continue;
                                }
                            }
                            if claude_chat.is_some() && (title.is_empty() || app_named) { continue; }
                            let id = claude_chat.unwrap_or_else(|| if title.is_empty() { "current".to_string() } else { title.to_string() });
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
                            let state = if generating {
                                "streaming"
                            } else if question {
                                "waitingQuestion"
                            } else if prev.as_ref().map(|p| p.0).unwrap_or(false) {
                                "done"
                            } else if prev.is_none() {
                                "idle"
                            } else {
                                "done"
                            };
                            // Other watched apps still require an observed generation.
                            if agent != "claude-desktop" && prev.is_none() && !generating && !question {
                                continue;
                            }
                            let changed = prev.as_ref() != Some(&(generating, question, text.clone(), title.to_string()));
                            if !changed {
                                continue;
                            }
                            last.insert(key, (generating, question, text.clone(), title.to_string()));
                            let fallback = core.lock().ok().and_then(|c| c.agent(agent).map(|a| a.name.clone())).unwrap_or_default();
                            let shown = if title.is_empty() { fallback.as_str() } else { title };
                            if let Ok(mut c) = core.lock() {
                                c.apply(Incoming {
                                    agent: agent.to_string(),
                                    event: None,
                                    body: json!({ "session": id, "state": state, "title": shown, "text": text, "_app": bundle }),
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

#[cfg(test)]
mod tests {
    use super::{claude_chat_id, current_conversation_running, message_child_start, recent_message_start, running_title, select_claude_webarea, ReplyObservation};

    #[test]
    fn only_a_finished_latest_assistant_question_waits() {
        let mut reply = ReplyObservation::default();
        reply.new_message();
        reply.heading("Claude responded: Choose");
        reply.text("On continue ?");
        assert!(reply.is_question(false));
        assert!(!reply.is_question(true), "streaming takes precedence");
        reply.text("Terminé.");
        assert!(!reply.is_question(false));
        reply.new_message();
        reply.heading("You said: pourquoi ?");
        reply.text("pourquoi ?");
        assert!(!reply.is_question(false), "latest user message is not an assistant question");
        reply.new_message();
        reply.text("Draft in composer?");
        assert!(!reply.is_question(false), "unattributed text is not an assistant question");
        reply.heading("ChatGPT said: question");
        reply.text("Yes?");
        assert!(reply.is_question(false), "short assistant questions are retained");
    }

    #[test]
    fn running_sidebar_label_requires_an_explicit_suffix() {
        assert_eq!(running_title("Example conversation Running"), Some("Example conversation"));
        assert_eq!(running_title("Running advice"), None);
        assert_eq!(running_title("Running"), None);
    }

    #[test]
    fn only_the_matching_claude_conversation_uses_the_sidebar_signal() {
        let running = vec!["Another conversation".to_string()];
        assert!(!current_conversation_running("claude-desktop", "Current conversation", &running));
        assert!(current_conversation_running("claude-desktop", "Another conversation", &running));
        assert!(!current_conversation_running("chatgpt", "Another conversation", &running));
        assert!(!current_conversation_running("claude-desktop", "", &running));
    }

    #[test]
    fn long_chat_history_keeps_latest_messages_and_other_containers_intact() {
        assert_eq!(recent_message_start("Chat messages", 20), 17);
        assert_eq!(recent_message_start("Chat messages", 2), 0);
        assert_eq!(recent_message_start("Sidebar", 20), 0);
    }

    #[test]
    fn long_history_leaves_budget_for_composer_after_transcript() {
        // The observed tree is Chat messages -> inner list -> many message nodes,
        // followed by the composer. Model a 3,500-node budget and 2,000 messages.
        let mut budget = 3500usize;
        let message_count = 2000usize;
        let first = message_child_start("", true, message_count);
        for _ in first..message_count { budget = budget.saturating_sub(5); }
        assert!(budget > 0, "composer remains reachable");
        assert_eq!(first, 1997);
    }

    #[test]
    fn claude_chat_id_accepts_only_https_chat_routes_on_the_exact_domain() {
        assert_eq!(claude_chat_id("https://claude.ai/chat/chat-123").as_deref(), Some("chat-123"));
        assert_eq!(claude_chat_id("https://claude.ai/chat/AbC123?source=desktop#reply").as_deref(), Some("AbC123"));
        for url in [
            "http://claude.ai/chat/chat-123",
            "https://claude.ai.evil.example/chat/chat-123",
            "https://evil-claude.ai/chat/chat-123",
            "https://claude.ai/code/session-123",
            "https://claude.ai/new",
            "https://claude.ai/settings",
            "https://claude.ai/projects",
            "https://claude.ai/chat/",
            "https://claude.ai/chat/not_a_valid_id",
        ] {
            assert_eq!(claude_chat_id(url), None, "unexpected chat ID from {url}");
        }
    }

    #[test]
    fn utility_web_areas_before_or_after_chat_never_replace_selected_chat() {
        let chat_url = "https://claude.ai/chat/chat-123";
        let utility_before = "https://claude.ai/code";
        let utility_after = "https://claude.ai/settings";

        let mut selected = None;
        select_claude_webarea(&mut selected, utility_before, "Claude Code");
        select_claude_webarea(&mut selected, chat_url, "A real conversation");
        select_claude_webarea(&mut selected, utility_after, "Settings");

        assert_eq!(selected, Some((chat_url.to_string(), "A real conversation".to_string())));
    }

    #[test]
    fn later_actual_chat_webarea_updates_selection() {
        let mut selected = None;
        select_claude_webarea(&mut selected, "https://claude.ai/chat/first", "First chat");
        select_claude_webarea(&mut selected, "https://claude.ai/chat/second", "Second chat");
        assert_eq!(selected, Some(("https://claude.ai/chat/second".into(), "Second chat".into())));
    }
}
