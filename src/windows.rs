use crate::macos::{self, _SLPSSetFrontProcessWithOptions, ProcessSerialNumber, make_key_window};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};

use objc2::rc::Retained;
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
#[allow(deprecated)]
use objc2_application_services::{AXError, AXUIElement, GetProcessForPID};
use objc2_core_foundation::{CFString, CGPoint, CGRect};
use objc2_core_graphics::{CGError, CGWarpMouseCursorPosition};

/// Incremented on every focus, so a background raise can tell it has been superseded.
static FOCUS_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Carries an AX element to the background focus thread. objc2 doesn't mark
/// `AXUIElement` as `Send`, but the Accessibility API may be called from any
/// thread (alt-tab-macos raises windows from a background queue too).
struct SendAx(Retained<AXUIElement>);

// SAFETY: see above. The element is used for a single AXRaise on the receiving
// thread and released there; CFRelease is thread safe.
unsafe impl Send for SendAx {}

impl SendAx {
    fn into_inner(self) -> Retained<AXUIElement> {
        self.0
    }
}

#[derive(Default)]
pub struct Manager {
    app_map: HashMap<i32, App>,
    ax_cache: HashMap<u32, macos::AxEntry>,
    icon_cache: HashMap<i32, macos::IconData>,
}

impl Manager {
    pub fn new() -> Result<Self> {
        let mut m = Self::default();
        m.refresh()?;
        Ok(m)
    }

    pub fn refresh(&mut self) -> Result<()> {
        let visible = macos::get_visible_window_ids();
        let window_infos =
            macos::get_window_info_list(&visible).context("Failed to get window info list")?;

        let active_pids: HashSet<i32> = window_infos.iter().map(|w| w.pid).collect();
        let active_wids: HashSet<u32> = window_infos.iter().map(|w| w.id).collect();

        let mut new_app_map = HashMap::new();
        let ws = NSWorkspace::sharedWorkspace();
        for app in ws.runningApplications() {
            let pid = app.processIdentifier();
            if !active_pids.contains(&pid) {
                continue;
            }
            if app.activationPolicy() != NSApplicationActivationPolicy::Regular
                || app.isTerminated()
            {
                continue;
            }
            let name = app
                .localizedName()
                .map(|n| n.to_string())
                .unwrap_or_default();

            if !self.icon_cache.contains_key(&pid)
                && let Some(data) = app.icon().and_then(|icon| macos::ns_image_to_rgba(&icon))
            {
                self.icon_cache.insert(pid, data);
            }

            new_app_map.insert(pid, App::new(app.clone(), name));
        }

        self.ax_cache.retain(|wid, _| active_wids.contains(wid));
        self.ax_cache.values_mut().for_each(macos::AxEntry::recheck);
        self.icon_cache.retain(|pid, _| active_pids.contains(pid));

        let mut uncached_by_pid: HashMap<i32, HashSet<u32>> = HashMap::new();
        for info in &window_infos {
            if !self.ax_cache.contains_key(&info.id) {
                uncached_by_pid.entry(info.pid).or_default().insert(info.id);
            }
        }

        for (pid, wids) in &uncached_by_pid {
            let resolved = macos::resolve_ax_for_pid(*pid, wids);
            self.ax_cache.extend(resolved);
        }

        for info in window_infos {
            let ax_element = match self.ax_cache.get(&info.id) {
                Some(macos::AxEntry::Switchable(element)) => Some(element.clone()),
                Some(macos::AxEntry::Rejected(_)) => continue,
                None => None,
            };
            if let Some(app) = new_app_map.get_mut(&info.pid) {
                app.windows.push(Window {
                    title: info.title,
                    id: info.id,
                    space_id: info.space_id,
                    display_uuid: info.display_uuid,
                    ax_element,
                });
            }
        }

        self.app_map = new_app_map;
        Ok(())
    }

    pub const fn app_map(&self) -> &HashMap<i32, App> {
        &self.app_map
    }

    pub fn get_icon(&self, pid: i32) -> Option<&macos::IconData> {
        self.icon_cache.get(&pid)
    }
}

#[derive(Debug)]
pub struct App {
    pub app: Retained<NSRunningApplication>,
    #[allow(dead_code)]
    pub pid: i32,
    pub name: String,
    pub windows: Vec<Window>,
}

impl App {
    pub fn new(app: Retained<NSRunningApplication>, name: String) -> Self {
        Self {
            pid: app.processIdentifier(),
            app,
            name,
            windows: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Window {
    pub title: String,
    pub id: u32,
    pub space_id: u64,
    pub display_uuid: Option<String>,
    ax_element: Option<Retained<AXUIElement>>,
}

impl Window {
    pub fn focus(&self, app: &NSRunningApplication) -> Result<()> {
        let cid = macos::SLSMainConnectionID();
        let mut rect = std::mem::MaybeUninit::<CGRect>::uninit();
        // SAFETY: `rect` is a valid out-pointer, and it is only read after
        // SkyLight reports success, which means it was written.
        let bounds = unsafe {
            let res = macos::SLSGetWindowBounds(cid, self.id, rect.as_mut_ptr());
            if res != CGError::Success {
                return Err(anyhow!("Could not get window bounds"));
            }
            rect.assume_init()
        };

        let center = CGPoint::new(
            bounds.origin.x + bounds.size.width / 2.,
            bounds.origin.y + bounds.size.height / 2.,
        );
        CGWarpMouseCursorPosition(center);

        if let Some(uuid) = self.display_uuid.as_deref() {
            // Raise the window within its own space before switching to it, so it is
            // already on top when the space appears. Otherwise the space's previous top
            // window shows until the front-process call below, after the wait.
            if macos::SLSGetActiveSpace(cid) != self.space_id
                && let Some(ax_element) = &self.ax_element
            {
                ax_raise(ax_element);
            }

            macos::switch_to_space_instant(self.space_id, uuid);
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
            while std::time::Instant::now() < deadline {
                if macos::SLSGetActiveSpace(cid) == self.space_id {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        } else {
            log::warn!(
                "window {} has no display UUID; skipping space switch",
                self.id
            );
        }

        let pid = app.processIdentifier();
        let mut psn = ProcessSerialNumber::default();

        // SAFETY: psn is a valid, correctly sized ProcessSerialNumber out-pointer.
        #[allow(deprecated)]
        let res = unsafe { GetProcessForPID(pid, (&mut psn as *mut _) as _) };

        if res != 0 {
            return Err(anyhow!("Couldn't get PSN for PID"));
        }

        // SAFETY: psn is a valid reference to a PSN filled in above.
        let res = unsafe { _SLPSSetFrontProcessWithOptions(&psn, self.id, 0x200) };
        if res != CGError::Success {
            return Err(anyhow!("Setting front process failed with: {res:?}"));
        }

        // The window is already fronted. Making it key and raising it are calls into the
        // target app (AXRaise can block up to the AX messaging timeout), so run them off
        // the main thread instead of keeping the picker up while they finish.
        let generation = FOCUS_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        let wid = self.id;
        let ax_element = self.ax_element.clone().map(SendAx);
        std::thread::spawn(move || {
            // A newer focus has started: finishing this one would cover its window.
            let superseded = || FOCUS_GENERATION.load(Ordering::SeqCst) != generation;

            if superseded() {
                return;
            }
            let res = make_key_window(wid, &psn);
            if res != CGError::Success {
                log::warn!("window {wid}: make_key_window failed with {res:?}");
            }

            if superseded() {
                return;
            }
            if let Some(ax_element) = ax_element.map(SendAx::into_inner) {
                ax_raise(&ax_element);
            }
        });

        Ok(())
    }
}

/// Asks the window's app to raise it, without activating the app.
fn ax_raise(element: &AXUIElement) -> AXError {
    // SAFETY: objc2 marks this unsafe by default; both arguments are
    // live references and the action name is a valid CFString.
    unsafe { AXUIElement::perform_action(element, &CFString::from_static_str("AXRaise")) }
}
