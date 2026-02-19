use std::ffi::c_void;
use std::{
    collections::{HashMap, HashSet},
    ptr::NonNull,
};

use anyhow::{anyhow, Result};

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSImage};
#[allow(deprecated)]
use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{
    CFArray, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGRect, CGSize,
    ConcreteType,
};
use objc2_core_graphics::CGWindowID;
use objc2_core_graphics::{
    kCGNullWindowID as NullID, kCGWindowLayer, kCGWindowName, kCGWindowNumber, kCGWindowOwnerPID,
    CGDataProvider, CGError, CGImage, CGWindowListCopyWindowInfo, CGWindowListOption as Options,
};

// Undocumented internal macos framework
#[link(name = "Skylight", kind = "framework")]
#[allow(dead_code)]
unsafe extern "C" {
    pub fn SLSMainConnectionID() -> u32;
    pub fn SLSGetActiveSpace(cid: u32) -> u64;
    fn SLSWindowIsOnSpace(cid: u32, window_id: CGWindowID, space_id: u64) -> bool;
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
    fn SLSOrderWindow(cid: u32, wid: u32, mode: i32, relative_to: u32) -> i32;
    fn SLSManagedDisplaySetCurrentSpace(
        cid: u32,
        display_uuid: *const c_void,
        space_id: u64,
    ) -> i32;
    fn SLSShowSpaces(cid: u32, space_ids: *const c_void) -> i32;
    pub fn SLSGetWindowBounds(cid: u32, wid: CGWindowID, bounds: *mut CGRect) -> CGError;
}

pub fn activate_application() {
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);
    app.activate();
}

pub fn hide_application() {
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);
    app.hide(None);
}

pub fn set_accessory_mode() {
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);
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

pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub pid: i32,
}

pub fn get_visible_window_ids() -> Result<HashSet<u32>> {
    let cid = unsafe { SLSMainConnectionID() };
    let dicts = unsafe {
        let ptr = NonNull::new_unchecked(SLSCopyManagedDisplaySpaces(cid) as *mut CFArray<CFDict>);
        CFRetained::from_raw(ptr)
    };

    let mut visible = HashSet::new();

    for display in dicts {
        let spaces = get_value_unchecked::<CFArray>(&display, &CFString::from_static_str("Spaces"));

        for space in unsafe { spaces.cast_unchecked::<CFDict>() } {
            let id = get_value_unchecked::<CFNumber>(&space, &CFString::from_static_str("id64"));

            let options = 0x2;
            let mut set_tags: u64 = 0;
            let mut clear_tags: u64 = 0;
            let space_ids = CFArray::from_retained_objects(std::slice::from_ref(&id));

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

            let arr = unsafe {
                let ptr = NonNull::new_unchecked(w_ptr as *mut CFArray<CFNumber>);
                CFRetained::from_raw(ptr)
            };

            for wid in arr {
                visible.insert(wid.as_i64().unwrap() as u32);
            }
        }
    }

    Ok(visible)
}

pub fn get_window_info_list(visible: &HashSet<u32>) -> Result<Vec<WindowInfo>> {
    let Some(window_list) = CGWindowListCopyWindowInfo(Options::ExcludeDesktopElements, NullID)
    else {
        return Err(anyhow!("CGWindowListCopyWindowInfo failed."));
    };

    let mut result = Vec::new();
    for dict in unsafe { window_list.cast_unchecked() } {
        let layer = get_value_unchecked::<CFNumber>(&dict, unsafe { kCGWindowLayer })
            .as_i32()
            .unwrap();
        let pid = get_value_unchecked::<CFNumber>(&dict, unsafe { kCGWindowOwnerPID })
            .as_i32()
            .unwrap();
        let title = get_value::<CFString>(&dict, unsafe { kCGWindowName })
            .map(|v| v.to_string())
            .unwrap_or_default();
        let window_number = get_value_unchecked::<CFNumber>(&dict, unsafe { kCGWindowNumber })
            .as_i64()
            .unwrap() as u32;

        if layer != 0 || !visible.contains(&window_number) {
            continue;
        }

        result.push(WindowInfo {
            id: window_number,
            title,
            pid,
        });
    }

    Ok(result)
}

pub fn resolve_ax_for_pid(
    pid: i32,
    target_wids: &HashSet<u32>,
) -> HashMap<u32, Retained<AXUIElement>> {
    let mut buffer = init_ax_buffer(pid);
    let mut cg_w_id = 0;
    let mut result = HashMap::new();
    let mut remaining: HashSet<u32> = target_wids.clone();

    for id in 0..100u64 {
        if remaining.is_empty() {
            break;
        }
        let ptr = ax_request(&mut buffer, id);
        if !ptr.is_null() {
            let element = unsafe { Retained::from_raw(ptr).unwrap() };
            if unsafe { _AXUIElementGetWindow(ptr as _, &mut cg_w_id) } != AXError::Success {
                continue;
            }

            if remaining.contains(&cg_w_id) && is_window(&element) {
                remaining.remove(&cg_w_id);
                result.insert(cg_w_id, element);
            }
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
    get_value(dict, value).unwrap_or_else(|| panic!("{} not found", value))
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

    let res = unsafe { SLPSPostEventRecordTo(psn, bytes.as_mut_ptr()) };
    if res != CGError::Success {
        return res;
    }

    bytes[0x08] = 0x02;
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
    unsafe {
        _AXUIElementCreateWithRemoteToken(CFRetained::as_ptr(&data).as_ptr() as _)
            as *mut AXUIElement
    }
}

pub fn get_ax_element(wid: u32, pid: i32) -> Option<Retained<AXUIElement>> {
    let mut buffer = init_ax_buffer(pid);
    let mut cg_id = 0;

    for id in 0..100u64 {
        let ptr = ax_request(&mut buffer, id);
        if !ptr.is_null() {
            let element = unsafe { Retained::from_raw(ptr).unwrap() };
            if unsafe { _AXUIElementGetWindow(ptr as _, &mut cg_id) } != AXError::Success {
                continue;
            }

            if cg_id == wid {
                return Some(element);
            }
        }
    }
    None
}

pub fn pid_from_ax(element: &AXUIElement) -> Option<u32> {
    let mut cg_id = 0;
    if unsafe { _AXUIElementGetWindow((element as *const _) as _, &mut cg_id) } != AXError::Success
    {
        return None;
    }

    Some(cg_id)
}

pub fn is_window(element: &AXUIElement) -> bool {
    if matches!(pid_from_ax(element), None | Some(0)) {
        return false;
    };

    let Some(subrole) = get_attribute(element, "AXSubrole") else {
        return false;
    };

    if let Ok(str) = subrole.downcast::<CFString>() {
        let string = str.to_string();
        return matches!(string.as_str(), "AXStandardWindow" | "AXDialog");
    }

    false
}

pub fn get_attribute(element: &AXUIElement, attr: &str) -> Option<CFRetained<CFType>> {
    let mut ptr: *const CFType = std::ptr::null();
    let attr = CFString::from_str(attr);
    let res = unsafe { element.copy_attribute_value(&attr, NonNull::new_unchecked(&mut ptr)) };
    if res != AXError::Success {
        eprintln!("AXUIElement::copy_attribute_value failed with {res:#?}");
        return None;
    }
    Some(unsafe { CFRetained::from_raw(NonNull::new(ptr as *mut CFType)?) })
}

#[allow(dead_code)]
fn focus_ax(wid: u32, pid: i32) {
    if let Some(el) = get_ax_element(wid, pid) {
        unsafe { AXUIElement::perform_action(&el, &CFString::from_static_str("AXRaise")) };
    }
}

pub struct IconData {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub fn ns_image_to_rgba(image: &NSImage) -> Option<IconData> {
    image.setSize(CGSize::new(16., 16.));

    let cg_image =
        unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) };

    if cg_image.is_none() {
        eprintln!("[icon] CGImageForProposedRect returned None");
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
            for chunk in raw_data.chunks_exact(3) {
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
            eprintln!("[icon] Unsupported bits_per_pixel: {other}");
            return None;
        }
    };

    Some(IconData {
        rgba,
        width,
        height,
    })
}
