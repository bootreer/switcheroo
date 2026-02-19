use crate::macos::{self, _SLPSSetFrontProcessWithOptions, ProcessSerialNumber, make_key_window};
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, anyhow};

use objc2::rc::Retained;
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
#[allow(deprecated)]
use objc2_application_services::{AXUIElement, GetProcessForPID};
use objc2_core_foundation::{CFString, CGPoint, CGRect};
use objc2_core_graphics::{CGError, CGWarpMouseCursorPosition};

#[derive(Default)]
pub struct Manager {
    app_map: HashMap<i32, App>,
    ax_cache: HashMap<u32, Retained<AXUIElement>>,
    icon_cache: HashMap<i32, macos::IconData>,
}

impl Manager {
    pub fn new() -> Result<Self> {
        let mut m = Self::default();
        m.refresh()?;
        Ok(m)
    }

    pub fn refresh(&mut self) -> Result<()> {
        let visible =
            macos::get_visible_window_ids().context("Failed to get visible window IDs")?;
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
            if let Some(ax_element) = self.ax_cache.get(&info.id)
                && let Some(app) = new_app_map.get_mut(&info.pid)
            {
                app.windows.push(Window {
                    title: info.title,
                    id: info.id,
                    ax_element: ax_element.clone(),
                });
            }
        }

        self.app_map = new_app_map;
        Ok(())
    }

    pub fn app_map(&self) -> &HashMap<i32, App> {
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
    ax_element: Retained<AXUIElement>,
}

impl Window {
    pub fn focus(&self, app: &NSRunningApplication) -> Result<()> {
        let pid = app.processIdentifier();
        let mut psn = ProcessSerialNumber::default();

        #[allow(deprecated)]
        let res = unsafe { GetProcessForPID(pid, (&mut psn as *mut _) as _) };

        if res != 0 {
            return Err(anyhow!("Couldn't get PSN for PID"));
        }

        let res = unsafe { _SLPSSetFrontProcessWithOptions(&psn, self.id, 0x200) };
        if res != CGError::Success {
            return Err(anyhow!("Setting front process failed with: {res:?}"));
        }

        let res = make_key_window(self.id, &psn);
        if res != CGError::Success {
            return Err(anyhow!("Failed at setting key window."));
        }

        unsafe {
            AXUIElement::perform_action(&self.ax_element, &CFString::from_static_str("AXRaise"))
        };

        let cid = unsafe { macos::SLSMainConnectionID() };
        let mut rect = std::mem::MaybeUninit::<CGRect>::uninit();
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

        Ok(())
    }
}
