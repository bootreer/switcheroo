use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    ptr::NonNull,
};

use egui::ColorImage;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplicationActivationOptions, NSApplicationActivationPolicy, NSImage, NSRunningApplication,
    NSWorkspace,
};

#[allow(deprecated)]
use objc2_application_services::{AXError, AXUIElement, GetProcessForPID};

use objc2_core_foundation::{
    CFArray, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect,
    ConcreteType, Type,
};
use objc2_core_graphics::{
    CGDataProvider, CGError, CGImage, CGRectMakeWithDictionaryRepresentation,
    CGWarpMouseCursorPosition, CGWindowListCopyWindowInfo, CGWindowListOption as Options,
    kCGNullWindowID as NullID, kCGWindowBounds, kCGWindowLayer, kCGWindowName, kCGWindowNumber,
    kCGWindowOwnerPID,
};

use anyhow::{Result, anyhow};

use objc2_core_graphics::CGWindowID;
use std::ffi::c_void;

// Undocumented internal macos framework
#[link(name = "Skylight", kind = "framework")]
#[allow(unused)]
unsafe extern "C" {
    fn SLSMainConnectionID() -> u32;
    fn SLSGetActiveSpace(c_id: u32) -> u64;
    fn SLSWindowIsOnSpace(c_id: u32, window_id: CGWindowID, space_id: u64) -> bool;
    fn SLSCopyManagedDisplaySpaces(c_id: u32) -> *mut c_void;
    fn SLSCopyWindowsWithOptionsAndTags(
        c_id: u32,
        owner: u32,
        spaces: *const c_void, // CFArray
        options: u32,

        // No idea what these are for
        set_tags: *mut u64,
        clear_tags: *mut u64,
    ) -> *const c_void;
    fn SLSOrderWindow(c_id: u32, w_id: u32, mode: i32, relative_to: u32) -> i32;
    fn SLSManagedDisplaySetCurrentSpace(
        c_id: u32,
        display_uuid: *const c_void,
        space_id: u64,
    ) -> i32;
    fn SLSShowSpaces(c_id: u32, space_ids: *const c_void) -> i32;
}

#[repr(C)]
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct ProcessSerialNumber {
    pub high_long_of_psn: u32,
    pub low_long_of_psn: u32,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn _AXUIElementCreateWithRemoteToken(data: *const c_void) -> *mut c_void;
    fn _AXUIElementGetWindow(element: *const c_void, cg_w_id: *mut CGWindowID) -> AXError;
    fn _SLPSSetFrontProcessWithOptions(
        psn: *const ProcessSerialNumber,
        w_id: CGWindowID,
        options: u32,
    ) -> CGError;
    fn SLPSPostEventRecordTo(psn: *const ProcessSerialNumber, bytes: *mut u8) -> CGError;
}

type CFDict = CFDictionary<CFString, CFType>;

#[derive(Debug)]
pub struct App {
    pub app: Retained<NSRunningApplication>,
    pub pid: i32,
    pub name: String,
    pub windows: Vec<Window>,
    pub icon: Option<ColorImage>,
}

impl App {
    fn new(app: Retained<NSRunningApplication>, name: String, icon: Option<ColorImage>) -> Self {
        Self {
            pid: app.processIdentifier(),
            app,
            name,
            windows: Vec::new(),
            icon,
        }
    }
}

#[derive(Debug)]
pub struct Window {
    pub title: String,
    pub id: i64,
    bounds: CGRect,
    #[allow(unused)]
    pub space: Space,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Space {
    pub display: u8,
    pub display_uuid: CFRetained<CFString>,
    pub index: Option<u8>,
    pub id: i64,
}

impl Hash for Window {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl PartialEq for Window {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Window {
    pub fn focus(&self, app: &NSRunningApplication) -> Result<()> {
        let mut psn = ProcessSerialNumber::default();
        let pid = app.processIdentifier();

        #[allow(deprecated)]
        let res = unsafe { GetProcessForPID(pid, (&mut psn as *mut _) as _) };

        if res != 0 {
            return Err(anyhow!("Couldn't get PSN for PID"));
        }

        let res = unsafe { _SLPSSetFrontProcessWithOptions(&psn, self.id as u32, 0x200) };
        if res != CGError::Success {
            return Err(anyhow!("Setting front process failed with: {res:?}"));
        }

        let res = self.make_key_window(&psn);
        if res != CGError::Success {
            return Err(anyhow!("Failed at setting key window."));
        }

        if self.space.id == unsafe { SLSGetActiveSpace(SLSMainConnectionID()) as i64 } {
            app.activateWithOptions(NSApplicationActivationOptions::all());
        } else {
            self.focus_ax(pid);
        }

        let center = CGPoint::new(
            self.bounds.origin.x + self.bounds.size.width / 2.,
            self.bounds.origin.y + self.bounds.size.height / 2.,
        );
        CGWarpMouseCursorPosition(center);

        Ok(())
    }

    fn make_key_window(&self, psn: &ProcessSerialNumber) -> CGError {
        let mut bytes = [0u8; 0xf8];

        bytes[0x04] = 0xf8;
        bytes[0x3a] = 0x10;

        let w_id_bytes = self.id.to_ne_bytes();
        bytes[0x3c] = w_id_bytes[0];
        bytes[0x3d] = w_id_bytes[1];
        bytes[0x3e] = w_id_bytes[2];
        bytes[0x3f] = w_id_bytes[3];

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

    // TODO: kinda slow
    fn focus_ax(&self, pid: i32) {
        let mut buffer = [0u8; 20];
        buffer[0..4].copy_from_slice(&pid.to_ne_bytes());
        buffer[4..8].copy_from_slice(&0i32.to_ne_bytes());
        buffer[8..12].copy_from_slice(&0x636f636fu32.to_ne_bytes());

        let mut cg_id = 0;

        // TODO: idk if 100 makes sense as an upper bound
        for id in 0..100u64 {
            buffer[12..20].copy_from_slice(&id.to_ne_bytes());
            let data = CFData::from_bytes(&buffer);
            let ptr = unsafe {
                _AXUIElementCreateWithRemoteToken(CFRetained::as_ptr(&data).as_ptr() as _)
                    as *mut AXUIElement
            };

            if !ptr.is_null() {
                let el = unsafe { Retained::retain(ptr).unwrap() };
                if unsafe { _AXUIElementGetWindow(Retained::as_ptr(&el) as _, &mut cg_id) }
                    != AXError::Success
                {
                    continue;
                }

                if cg_id == self.id as u32 {
                    unsafe {
                        AXUIElement::perform_action(&el, &CFString::from_static_str("AXRaise"))
                    };
                    return;
                }
            }
        }
    }
}

pub fn get_open_app_windows() -> Result<HashMap<i32, App>> {
    let mut app_map = get_apps();

    let c_id = unsafe { SLSMainConnectionID() };
    let dict = unsafe {
        let ptr = NonNull::new_unchecked(SLSCopyManagedDisplaySpaces(c_id) as *mut CFArray<CFDict>);
        CFRetained::from_raw(ptr)
    };

    let mut visible = HashMap::new();
    let mut cnt = 0;

    for (i, display) in dict.iter().enumerate() {
        let spaces = get_value_unchecked::<CFArray>(&display, &CFString::from_static_str("Spaces"));
        let uuid = get_value_unchecked::<CFString>(
            &display,
            &CFString::from_static_str("Display Identifier"),
        );

        for space in unsafe { spaces.cast_unchecked::<CFDict>() } {
            let id = get_value_unchecked::<CFNumber>(&space, &CFString::from_static_str("id64"));

            let index = {
                let space_type =
                    get_value_unchecked::<CFNumber>(&space, &CFString::from_static_str("type"))
                        .as_i64()
                        .unwrap();
                if space_type == 0 {
                    cnt += 1;
                    Some(cnt)
                } else {
                    None
                }
            };

            let options = 0x2;
            let mut set_tags: u64 = 0;
            let mut clear_tags: u64 = 0;
            let space_ids = CFArray::from_retained_objects(std::slice::from_ref(&id));

            let w_ptr = unsafe {
                SLSCopyWindowsWithOptionsAndTags(
                    c_id,
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

            for w_id in arr {
                visible.insert(
                    w_id.as_i64().unwrap(),
                    Space {
                        display: i as u8 + 1,
                        display_uuid: uuid.retain(),
                        index,
                        id: id.as_i64().unwrap(),
                    },
                );
            }
        }
    }

    let Some(window_list) = CGWindowListCopyWindowInfo(Options::ExcludeDesktopElements, NullID)
    else {
        return Err(anyhow!("CGWindowListCopyWindowInfo failed."));
    };

    let mut all_windows = HashSet::new();
    for dict in unsafe { window_list.cast_unchecked() } {
        let layer: i32 = get_value_unchecked::<CFNumber>(&dict, unsafe { kCGWindowLayer })
            .as_i32()
            .unwrap();
        let app_pid = get_value_unchecked::<CFNumber>(&dict, unsafe { kCGWindowOwnerPID })
            .as_i32()
            .unwrap();
        let title = get_value::<CFString>(&dict, unsafe { kCGWindowName })
            .map(|v| v.to_string())
            .unwrap_or_default();
        let window_number = get_value_unchecked::<CFNumber>(&dict, unsafe { kCGWindowNumber })
            .as_i64()
            .unwrap();

        if layer != 0 || !app_map.contains_key(&app_pid) || !visible.contains_key(&window_number) {
            continue;
        }

        let bounds = {
            let mut rect = std::mem::MaybeUninit::<CGRect>::uninit();
            let dict = get_value_unchecked::<CFDictionary>(&dict, unsafe { kCGWindowBounds });
            if unsafe {
                CGRectMakeWithDictionaryRepresentation(Some(dict.as_ref()), rect.as_mut_ptr())
            } {
                unsafe { rect.assume_init() }
            } else {
                return Err(anyhow!("CGRectMakeWithDictionaryRepresentation failed."));
            }
        };

        all_windows.insert(window_number);
        app_map.entry(app_pid).and_modify(|app| {
            app.windows.push(Window {
                title,
                bounds,
                id: window_number,
                space: visible.remove(&window_number).unwrap(),
            });
        });
    }

    Ok(app_map)
}

fn get_apps() -> HashMap<i32, App> {
    use objc2::Message;
    let mut app_map = HashMap::<i32, App>::new();

    let ws = NSWorkspace::sharedWorkspace();
    for app in ws.runningApplications() {
        let pid = app.processIdentifier();
        if app.activationPolicy() != NSApplicationActivationPolicy::Regular || app.isTerminated() {
            continue;
        }

        let name = app
            .localizedName()
            .map(|n| n.to_string())
            .unwrap_or_default();

        app_map.insert(
            pid,
            App::new(
                app.retain(),
                name,
                app.icon().and_then(|icon| ns_image_to_color(&icon)),
            ),
        );
    }
    app_map
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

fn ns_image_to_color(image: &NSImage) -> Option<ColorImage> {
    let cg_image =
        unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) };

    let width = CGImage::width(cg_image.as_deref()) as usize;
    let height = CGImage::height(cg_image.as_deref()) as usize;
    let bytes_per_row = CGImage::bytes_per_row(cg_image.as_deref()) as usize;
    let bits_per_pixel = CGImage::bits_per_pixel(cg_image.as_deref());
    // let bitmap_info = CGImage::bitmap_info(cg_image.as_deref());
    // let alpha_info = CGImage::alpha_info(cg_image.as_deref());

    let data_provider = CGImage::data_provider(cg_image.as_deref());
    let data = CGDataProvider::data(data_provider.as_deref())?;
    let raw_data = data.to_vec();

    // TODO: Not sure if all possibilities are handled correctly/at all
    match bits_per_pixel {
        24 => Some(ColorImage::from_rgb([width, height], &raw_data)),
        32 => Some(ColorImage::from_rgba_unmultiplied(
            [width, height],
            &raw_data,
        )),
        64 => {
            let mut rgba = Vec::with_capacity(width * height * 4);
            for y in 0..height {
                for x in 0..width {
                    let offset = y * bytes_per_row + x * 8;
                    if offset + 7 < raw_data.len() {
                        let r = half::f16::from_le_bytes([raw_data[offset], raw_data[offset + 1]]);
                        let g =
                            half::f16::from_le_bytes([raw_data[offset + 2], raw_data[offset + 3]]);
                        let b =
                            half::f16::from_le_bytes([raw_data[offset + 4], raw_data[offset + 5]]);
                        let a =
                            half::f16::from_le_bytes([raw_data[offset + 6], raw_data[offset + 7]]);

                        // Convert f16 (0.0-1.0) to u8 (0-255)
                        rgba.push((r.to_f32().clamp(0.0, 1.0) * 255.0) as u8);
                        rgba.push((g.to_f32().clamp(0.0, 1.0) * 255.0) as u8);
                        rgba.push((b.to_f32().clamp(0.0, 1.0) * 255.0) as u8);
                        rgba.push((a.to_f32().clamp(0.0, 1.0) * 255.0) as u8);
                    }
                }
            }
            Some(ColorImage::from_rgba_premultiplied([width, height], &rgba))
        }
        _ => None,
    }
}
