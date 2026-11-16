use std::ffi::c_void;
use std::time::{Duration, Instant};
use std::{
    collections::{HashMap, HashSet},
    ptr::NonNull,
};

use anyhow::{Result, anyhow};

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSImage};
use objc2_application_services::{
    AXError, AXIsProcessTrustedWithOptions, AXUIElement, kAXTrustedCheckOptionPrompt,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGRect,
    CGSize, ConcreteType,
};
use objc2_core_graphics::{
    CGDataProvider, CGDisplayBounds, CGError, CGEvent, CGGetDisplaysWithPoint, CGImage,
    CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess, CGWindowID,
    CGWindowListCopyWindowInfo, CGWindowListOption as Options, kCGNullWindowID as NullID,
    kCGWindowLayer, kCGWindowName, kCGWindowNumber, kCGWindowOwnerPID,
};

// Undocumented internal macos framework
#[link(name = "Skylight", kind = "framework")]
unsafe extern "C" {
    // SAFETY: these take and return plain integers; an invalid connection id
    // yields 0, not undefined behaviour.
    pub safe fn SLSMainConnectionID() -> u32;
    pub safe fn SLSGetActiveSpace(cid: u32) -> u64;
    fn SLSCopyManagedDisplaySpaces(cid: u32) -> *mut c_void;
    fn SLSCopyWindowsWithOptionsAndTags(
        cid: u32,
        owner: u32,
        spaces: *const c_void, // CFArray
        options: u32,

        // No idea what these are for
        set_tags: *mut u64,
        clear_tags: *mut u64,
    ) -> *const c_void;
    pub fn SLSGetWindowBounds(cid: u32, wid: CGWindowID, bounds: *mut CGRect) -> CGError;
}

pub fn switch_to_space_instant(target_space_id: u64, display_uuid: &str) {
    let mut current_space_id: Option<u64> = None;
    let mut ordered_space_ids: Vec<u64> = Vec::new();

    let Some(displays) = copy_managed_display_spaces() else {
        return;
    };
    for display in dicts(&displays) {
        if display_uuid_of(&display).as_deref() != Some(display_uuid) {
            continue;
        }

        if let Some(current_dict) =
            get_value::<CFDictionary>(&display, &CFString::from_static_str("Current Space"))
        {
            let current_dict = as_cf_dict(current_dict);
            if let Some(id) =
                get_value::<CFNumber>(&current_dict, &CFString::from_static_str("id64"))
                && let Some(v) = id.as_i64()
            {
                current_space_id = Some(v as u64);
            }
        }

        let spaces = get_value_unchecked::<CFArray>(&display, &CFString::from_static_str("Spaces"));
        for space in dicts(&spaces) {
            if let Some(id) = get_value::<CFNumber>(&space, &CFString::from_static_str("id64"))
                && let Some(v) = id.as_i64()
            {
                ordered_space_ids.push(v as u64);
            }
        }

        break;
    }

    let Some(current_id) = current_space_id else {
        return;
    };
    if current_id == target_space_id {
        return;
    }

    let Some(current_idx) = ordered_space_ids.iter().position(|&id| id == current_id) else {
        return;
    };
    let Some(target_idx) = ordered_space_ids
        .iter()
        .position(|&id| id == target_space_id)
    else {
        return;
    };

    let is_right = target_idx > current_idx;
    let hops = target_idx.abs_diff(current_idx);
    crate::dock_swipe::switch_spaces(is_right, hops);
}

fn main_thread() -> MainThreadMarker {
    MainThreadMarker::new().expect("AppKit must be called from the main thread")
}

pub fn activate_application() {
    let app = NSApplication::sharedApplication(main_thread());
    app.activate();
}

pub fn hide_application() {
    let app = NSApplication::sharedApplication(main_thread());
    app.hide(None);
}

/// Returns (x, y, width, height) of the display containing the cursor,
/// in the global display coordinate space (top-left of primary display is origin, Y downward).
pub fn active_display_frame_at_cursor() -> Option<(f32, f32, f32, f32)> {
    let ev = CGEvent::new(None)?;
    let loc = CGEvent::location(Some(&ev));

    let mut display = 0u32;
    let mut count = 0u32;
    // SAFETY: both out-pointers are valid locals and max_displays is 1, so at
    // most one id is written to `display`.
    unsafe { CGGetDisplaysWithPoint(loc, 1, &mut display, &mut count) };

    if count == 0 {
        return None;
    }

    let b = CGDisplayBounds(display);
    Some((
        b.origin.x as f32,
        b.origin.y as f32,
        b.size.width as f32,
        b.size.height as f32,
    ))
}

pub fn set_accessory_mode() {
    let app = NSApplication::sharedApplication(main_thread());
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
}

#[repr(C)]
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct ProcessSerialNumber {
    high_long_of_psn: u32,
    low_long_of_psn: u32,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn _AXUIElementCreateWithRemoteToken(data: *const c_void) -> *mut c_void;
    fn _AXUIElementGetWindow(element: *const c_void, cg_w_id: *mut CGWindowID) -> AXError;
    pub fn _SLPSSetFrontProcessWithOptions(
        psn: *const ProcessSerialNumber,
        wid: CGWindowID,
        options: u32,
    ) -> CGError;
    fn SLPSPostEventRecordTo(psn: *const ProcessSerialNumber, bytes: *mut u8) -> CGError;
}

type CFDict = CFDictionary<CFString, CFType>;

fn as_cf_dict(dict: CFRetained<CFDictionary>) -> CFRetained<CFDict> {
    // SAFETY: CFDictionary's key and value parameters are phantom: lookups go
    // through CFEqual/CFHash on any CF object, and get_value type-checks every
    // value it reads before use.
    unsafe { CFRetained::cast_unchecked(dict) }
}

/// The dictionary elements of `array`, skipping any element of another type.
fn dicts(array: &CFArray) -> impl Iterator<Item = CFRetained<CFDict>> + '_ {
    // SAFETY: every CFArray element is a CF object, which is all CFType claims.
    let items = unsafe { array.cast_unchecked::<CFType>() };
    items
        .iter()
        .filter_map(|item| item.downcast::<CFDictionary>().ok().map(as_cf_dict))
}

fn copy_managed_display_spaces() -> Option<CFRetained<CFArray>> {
    // SAFETY: takes a connection id by value and returns a +1 (Copy rule)
    // CFArray or NULL.
    let ptr = unsafe { SLSCopyManagedDisplaySpaces(SLSMainConnectionID()) };
    let Some(ptr) = NonNull::new(ptr.cast::<CFArray>()) else {
        log::warn!("SLSCopyManagedDisplaySpaces returned NULL");
        return None;
    };
    // SAFETY: non-null and owned (+1), so CFRetained takes over that retain.
    Some(unsafe { CFRetained::from_raw(ptr) })
}

fn display_uuid_of(display: &CFDict) -> Option<String> {
    get_value::<CFString>(display, &CFString::from_static_str("Display Identifier"))
        .map(|v| v.to_string())
}

pub struct WindowLocation {
    pub space_id: u64,
    pub display_uuid: Option<String>,
}

#[derive(Debug)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub pid: i32,
    pub space_id: u64,
    pub display_uuid: Option<String>,
}

pub fn get_visible_window_ids() -> HashMap<u32, WindowLocation> {
    let cid = SLSMainConnectionID();
    let mut visible = HashMap::new();
    let Some(displays) = copy_managed_display_spaces() else {
        return visible;
    };

    for display in dicts(&displays) {
        let display_uuid = display_uuid_of(&display);
        if display_uuid.is_none() {
            log::warn!("missing Display Identifier in SLSCopyManagedDisplaySpaces dict");
        }

        let spaces = get_value_unchecked::<CFArray>(&display, &CFString::from_static_str("Spaces"));

        for space in dicts(&spaces) {
            let id = get_value_unchecked::<CFNumber>(&space, &CFString::from_static_str("id64"));
            let Some(space_id) = id.as_i64().map(|v| v as u64) else {
                continue;
            };

            let options = 0x2;
            let mut set_tags: u64 = 0;
            let mut clear_tags: u64 = 0;
            let space_ids = CFArray::from_retained_objects(std::slice::from_ref(&id));

            // SAFETY: space_ids is a live CFArray for the call and both tag
            // pointers are valid locals. Returns a +1 CFArray or NULL.
            let w_ptr = unsafe {
                SLSCopyWindowsWithOptionsAndTags(
                    cid,
                    0,
                    CFRetained::as_ptr(&space_ids).as_ptr() as _,
                    options,
                    &mut set_tags,
                    &mut clear_tags,
                )
            };

            let Some(w_ptr) = NonNull::new(w_ptr as *mut CFArray) else {
                log::warn!("SLSCopyWindowsWithOptionsAndTags returned NULL for space {space_id}");
                continue;
            };
            // SAFETY: non-null and owned (+1), so CFRetained takes over that retain.
            let arr = unsafe { CFRetained::from_raw(w_ptr) };
            // SAFETY: every CFArray element is a CF object.
            let items = unsafe { arr.cast_unchecked::<CFType>() };

            for wid in items {
                if let Some(wid) = wid.downcast::<CFNumber>().ok().and_then(|n| n.as_i64()) {
                    visible.insert(
                        wid as u32,
                        WindowLocation {
                            space_id,
                            display_uuid: display_uuid.clone(),
                        },
                    );
                }
            }
        }
    }

    visible
}

pub fn get_window_info_list(visible: &HashMap<u32, WindowLocation>) -> Result<Vec<WindowInfo>> {
    let Some(window_list) = CGWindowListCopyWindowInfo(Options::ExcludeDesktopElements, NullID)
    else {
        return Err(anyhow!("CGWindowListCopyWindowInfo failed."));
    };

    // SAFETY: immutable CFString constants exported by CoreGraphics.
    let (layer_key, number_key, pid_key, name_key) = unsafe {
        (
            kCGWindowLayer,
            kCGWindowNumber,
            kCGWindowOwnerPID,
            kCGWindowName,
        )
    };
    let mut result = Vec::new();
    for dict in dicts(&window_list) {
        let layer = get_value_unchecked::<CFNumber>(&dict, layer_key)
            .as_i32()
            .unwrap();
        if layer != 0 {
            continue;
        }

        let window_number = get_value_unchecked::<CFNumber>(&dict, number_key)
            .as_i64()
            .unwrap() as u32;

        let Some(loc) = visible.get(&window_number) else {
            continue;
        };

        let pid = get_value_unchecked::<CFNumber>(&dict, pid_key)
            .as_i32()
            .unwrap();
        // Untitled = helper window (e.g. fullscreen toolbar strip)
        let Some(title) = get_value::<CFString>(&dict, name_key)
            .map(|v| v.to_string())
            .filter(|t| !t.is_empty())
        else {
            continue;
        };

        result.push(WindowInfo {
            id: window_number,
            title,
            pid,
            space_id: loc.space_id,
            display_uuid: loc.display_uuid.clone(),
        });
    }

    Ok(result)
}

pub enum AxEntry {
    Switchable(Retained<AXUIElement>),
    /// Kept so `recheck` can promote it if the subrole changes (e.g. Books)
    Rejected(Retained<AXUIElement>),
}

impl AxEntry {
    pub fn recheck(&mut self) {
        if let Self::Rejected(element) = self
            && window_subrole(element).is_some_and(|s| is_switchable_subrole(&s))
        {
            *self = Self::Switchable(element.clone());
        }
    }
}

/// Resolves AX elements for `target_wids`; unresolved wids are absent.
pub fn resolve_ax_for_pid(pid: i32, target_wids: &HashSet<u32>) -> HashMap<u32, AxEntry> {
    let mut result = HashMap::new();
    let consider = |result: &mut HashMap<_, _>, element: Retained<AXUIElement>| {
        let Some(wid) = window_id_from_ax(&element) else {
            return;
        };
        if !target_wids.contains(&wid) || result.contains_key(&wid) {
            return;
        }
        let Some(subrole) = window_subrole(&element) else {
            return;
        };
        let entry = if is_switchable_subrole(&subrole) {
            AxEntry::Switchable(element)
        } else {
            log::debug!("hiding window {wid} (pid {pid}): subrole {subrole}");
            AxEntry::Rejected(element)
        };
        result.insert(wid, entry);
    };
    // SAFETY: objc2 marks this unsafe by default; it takes a pid by value and
    // returns an element even for a dead pid (requests on it then fail).
    let app = unsafe { AXUIElement::new_application(pid) };

    // Fast path 1: ask the application AX element for its window list
    // Note: AXWindows only covers the current Space and macOS sometimes
    // returns a stale or empty list, so it is a fast path, not the truth.
    if let Some(windows) = get_attribute::<CFArray>(&app, "AXWindows") {
        // SAFETY: every CFArray element is a CF object.
        for value in unsafe { windows.cast_unchecked::<CFType>() } {
            if result.len() == target_wids.len() {
                return result;
            }
            if let Ok(element) = value.downcast::<AXUIElement>() {
                consider(&mut result, element.into());
            }
        }
    }

    // Fast path 2: some apps (Electron/Slack, SwiftUI terminals like Ghostty)
    // report an empty or stale AXWindows list even while windows exist, but
    // do answer AXFocusedWindow/AXMainWindow with the live window element.
    // Each refresh catches whichever window is active; the caller's cache
    // accumulates the rest across refreshes.
    for attr in ["AXFocusedWindow", "AXMainWindow"] {
        if result.len() == target_wids.len() {
            return result;
        }
        if let Some(element) = get_attribute::<AXUIElement>(&app, attr) {
            consider(&mut result, element.into());
        }
    }

    // Brute-force fallback. Window IDs can be very high (Chrome, Ghostty),
    // so bound by time, not an ID cap.
    let mut buffer = init_ax_buffer(pid);
    let budget = Duration::from_millis((50 + 25 * target_wids.len() as u64).min(500));
    let deadline = Instant::now() + budget;

    let mut id = 0u64;
    while result.len() < target_wids.len() && Instant::now() < deadline {
        let ptr = ax_request(&mut buffer, id);
        id += 1;
        // SAFETY: _AXUIElementCreateWithRemoteToken follows the Create rule,
        // so a non-null result is an owned (+1) AXUIElement.
        if let Some(element) = unsafe { Retained::from_raw(ptr) } {
            consider(&mut result, element);
        }
    }

    result
}

fn get_value<T: ConcreteType>(
    dict: &CFDictionary<CFString, CFType>,
    value: &CFString,
) -> Option<CFRetained<T>> {
    dict.get(value)?.downcast::<T>().ok()
}

fn get_value_unchecked<T: ConcreteType>(
    dict: &CFDictionary<CFString, CFType>,
    value: &CFString,
) -> CFRetained<T> {
    get_value(dict, value).unwrap_or_else(|| panic!("{value} not found"))
}

pub fn make_key_window(id: u32, psn: &ProcessSerialNumber) -> CGError {
    let mut bytes = [0u8; 0xf8];

    bytes[0x04] = 0xf8;
    bytes[0x3a] = 0x10;

    let wid_bytes = id.to_ne_bytes();
    bytes[0x3c] = wid_bytes[0];
    bytes[0x3d] = wid_bytes[1];
    bytes[0x3e] = wid_bytes[2];
    bytes[0x3f] = wid_bytes[3];

    bytes[0x20..0x30].fill(0xff);

    bytes[0x08] = 0x01;

    // SAFETY: psn is a valid reference and `bytes` is the full 0xf8-byte
    // event record SkyLight reads (size stored at offset 0x04).
    let res = unsafe { SLPSPostEventRecordTo(psn, bytes.as_mut_ptr()) };
    if res != CGError::Success {
        return res;
    }

    bytes[0x08] = 0x02;
    // SAFETY: as above.
    let res = unsafe { SLPSPostEventRecordTo(psn, bytes.as_mut_ptr()) };
    if res != CGError::Success {
        return res;
    }
    CGError::Success
}

fn init_ax_buffer(pid: i32) -> [u8; 20] {
    let mut buffer = [0u8; 20];
    buffer[0..4].copy_from_slice(&pid.to_ne_bytes());
    buffer[4..8].copy_from_slice(&0i32.to_ne_bytes());
    buffer[8..12].copy_from_slice(&0x636f636fu32.to_ne_bytes());
    buffer
}

fn ax_request(buffer: &mut [u8; 20], id: u64) -> *mut AXUIElement {
    buffer[12..20].copy_from_slice(&id.to_ne_bytes());
    let data = CFData::from_bytes(buffer);
    // SAFETY: `data` is a live CFData for the duration of the call; the
    // returned element is owned by the caller (Create rule) or NULL.
    unsafe {
        _AXUIElementCreateWithRemoteToken(CFRetained::as_ptr(&data).as_ptr() as _)
            as *mut AXUIElement
    }
}

pub fn window_id_from_ax(element: &AXUIElement) -> Option<u32> {
    let mut cg_id = 0;
    // SAFETY: `element` is a live AXUIElement and cg_id a valid out-pointer.
    if unsafe { _AXUIElementGetWindow((element as *const _) as _, &mut cg_id) } != AXError::Success
    {
        return None;
    }

    Some(cg_id)
}

/// `None` for child elements (they share the window's ID) or unreadable attributes
fn window_subrole(element: &AXUIElement) -> Option<String> {
    let role = get_attribute::<CFString>(element, "AXRole")?.to_string();
    if role != "AXWindow" {
        return None;
    }
    Some(get_attribute::<CFString>(element, "AXSubrole")?.to_string())
}

fn is_switchable_subrole(subrole: &str) -> bool {
    matches!(subrole, "AXStandardWindow" | "AXDialog")
}

pub fn get_attribute<T: ConcreteType>(element: &AXUIElement, attr: &str) -> Option<CFRetained<T>> {
    let mut ptr: *const CFType = std::ptr::null();
    let attr = CFString::from_str(attr);
    // SAFETY: the out-pointer comes from a live local, so it is valid and non-null.
    let res = unsafe { element.copy_attribute_value(&attr, NonNull::from(&mut ptr)) };
    if res != AXError::Success {
        log::trace!("copy_attribute_value({attr}) failed with {res:?}");
        return None;
    }
    // SAFETY: on success the value is returned +1 (Copy rule); NULL is handled by `?`.
    let value = unsafe { CFRetained::from_raw(NonNull::new(ptr as *mut CFType)?) };
    value.downcast::<T>().ok()
}

pub struct IconData {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

// TODO: why not just render the CGImage?
pub fn ns_image_to_rgba(image: &NSImage) -> Option<IconData> {
    image.setSize(CGSize::new(16., 16.));

    // SAFETY: a NULL proposed rect is documented as "use the image's size",
    // and context/hints are optional.
    let cg_image =
        unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) };

    if cg_image.is_none() {
        log::warn!("CGImageForProposedRect returned None");
        return None;
    }

    let width = CGImage::width(cg_image.as_deref()) as u32;
    let height = CGImage::height(cg_image.as_deref()) as u32;
    let bytes_per_row = CGImage::bytes_per_row(cg_image.as_deref()) as usize;
    let bits_per_pixel = CGImage::bits_per_pixel(cg_image.as_deref());

    let data_provider = CGImage::data_provider(cg_image.as_deref());
    let data = CGDataProvider::data(data_provider.as_deref())?;
    let raw_data = data.to_vec();

    // Convert to RGBA8 regardless of source format
    let rgba = match bits_per_pixel {
        24 => {
            // RGB -> RGBA
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for chunk in raw_data.as_chunks::<3>().0 {
                rgba.extend_from_slice(chunk);
                rgba.push(255);
            }
            rgba
        }
        32 => raw_data,
        64 => {
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let offset = y * bytes_per_row + x * 8;
                    if offset + 7 < raw_data.len() {
                        let r = half::f16::from_le_bytes([raw_data[offset], raw_data[offset + 1]]);
                        let g =
                            half::f16::from_le_bytes([raw_data[offset + 2], raw_data[offset + 3]]);
                        let b =
                            half::f16::from_le_bytes([raw_data[offset + 4], raw_data[offset + 5]]);
                        let a =
                            half::f16::from_le_bytes([raw_data[offset + 6], raw_data[offset + 7]]);

                        rgba.push((r.to_f32().clamp(0.0, 1.0) * 255.0) as u8);
                        rgba.push((g.to_f32().clamp(0.0, 1.0) * 255.0) as u8);
                        rgba.push((b.to_f32().clamp(0.0, 1.0) * 255.0) as u8);
                        rgba.push((a.to_f32().clamp(0.0, 1.0) * 255.0) as u8);
                    }
                }
            }
            rgba
        }
        other => {
            log::warn!("unsupported icon bits_per_pixel: {other}");
            return None;
        }
    };

    Some(IconData {
        rgba,
        width,
        height,
    })
}

/// Requests Accessibility and Screen Recording (for titles), opening Settings for any missing
pub fn request_permissions() -> bool {
    let prompt = CFDictionary::from_slices(
        // SAFETY: immutable CFString constant exported by ApplicationServices.
        &[unsafe { kAXTrustedCheckOptionPrompt }],
        &[CFBoolean::new(true)],
    );
    // SAFETY: the options dictionary has the documented CFString -> CFBoolean shape.
    let accessibility = unsafe { AXIsProcessTrustedWithOptions(Some(prompt.as_opaque())) };
    let screen_recording = CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess();

    for (granted, name, pane) in [
        (accessibility, "Accessibility", "Privacy_Accessibility"),
        (
            screen_recording,
            "Screen Recording",
            "Privacy_ScreenCapture",
        ),
    ] {
        if granted {
            continue;
        }
        log::error!(
            "{name} permission is required: grant it in System Settings, then relaunch switcheroo"
        );
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
        if let Err(e) = std::process::Command::new("open").arg(url).status() {
            log::error!("failed to open System Settings: {e}");
        }
    }

    accessibility && screen_recording
}
