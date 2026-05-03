#![cfg(target_os = "macos")]
#![allow(non_snake_case, unused_unsafe)]

use std::cell::RefCell;
use std::path::PathBuf;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEvent,
    NSImage, NSImageView, NSMenu, NSMenuItem, NSPanel, NSRunningApplication, NSScreen,
    NSStatusBar, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSBundle, NSDictionary, NSNumber, NSPoint,
    NSRect, NSRunLoop, NSRunLoopMode, NSSize, NSString, NSTimer,
};

use crate::manifest;

// ---- CoreGraphics / Foundation FFI ----

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: u32) -> *mut AnyObject;
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    static NSRunLoopCommonModes: *const std::ffi::c_void;
}

const OPT_ON_SCREEN: u32 = 1 << 0;
const OPT_EXCL_DESKTOP: u32 = 1 << 4;
const NULL_WINDOW: u32 = 0;

// ---- App state ----

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Corner,
    Wall,
}

struct State {
    panel: Retained<NSPanel>,
    corner_image: Retained<NSImage>,
    wall_image: Retained<NSImage>,
    corner_anchor_x: f64,
    corner_anchor_y: f64,
    wall_anchor_x: f64,
    mode: Mode,
    _status_item: Retained<objc2_app_kit::NSStatusItem>,
    _timer: Retained<NSTimer>,
}

thread_local! {
    static APP: RefCell<Option<State>> = RefCell::new(None);
}

// ---- Asset loading ----

/// Returns the bearded_dragon directory.
/// - .app bundle: Contents/Resources/assets/bearded_dragon/
/// - dev fallback: <exe>/../../../../assets/bearded_dragon/
pub fn char_dir() -> Option<PathBuf> {
    let bundle_path = unsafe {
        let bundle = NSBundle::mainBundle();
        bundle
            .resourceURL()
            .and_then(|base| {
                let r = NSString::from_str("assets/bearded_dragon");
                base.URLByAppendingPathComponent(&r)
            })
            .and_then(|url| url.path())
            .map(|p| PathBuf::from(p.to_string()))
            .filter(|p| p.exists())
    };
    if let Some(p) = bundle_path {
        return Some(p);
    }
    let exe = std::env::current_exe().ok()?;
    exe.parent()?
        .join("../../../../assets/bearded_dragon")
        .canonicalize()
        .ok()
}

fn load_img(char_dir: &std::path::Path, name: &str) -> Option<Retained<NSImage>> {
    let path = char_dir.join("sprite").join(name);
    let s = NSString::from_str(path.to_str()?);
    unsafe { NSImage::initWithContentsOfFile(NSImage::alloc(), &s) }
}

#[allow(deprecated)]
fn scale_img(src: &NSImage, scale: f64) -> Retained<NSImage> {
    let orig = unsafe { src.size() };
    let sz = NSSize::new(orig.width * scale, orig.height * scale);
    unsafe {
        let dst = NSImage::initWithSize(NSImage::alloc(), sz);
        dst.lockFocus();
        src.drawInRect(NSRect::new(NSPoint::ZERO, sz));
        dst.unlockFocus();
        dst
    }
}

// ---- Panel helpers ----

fn make_iv(image: &NSImage, mt: MainThreadMarker) -> Retained<NSImageView> {
    let sz = unsafe { image.size() };
    unsafe {
        let iv = NSImageView::initWithFrame(
            NSImageView::alloc(mt),
            NSRect::new(NSPoint::ZERO, sz),
        );
        iv.setImage(Some(image));
        iv
    }
}

fn make_panel(corner_image: &NSImage, mt: MainThreadMarker) -> Retained<NSPanel> {
    let sz = unsafe { corner_image.size() };
    unsafe {
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mt),
            NSRect::new(NSPoint::ZERO, sz),
            NSWindowStyleMask::from_bits_retain(128), // Borderless | NonactivatingPanel
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setOpaque(false);
        panel.setHasShadow(false);
        panel.setLevel(3); // NSFloatingWindowLevel
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        panel.setIgnoresMouseEvents(true);
        panel.setContentView(Some(&*make_iv(corner_image, mt)));
        panel
    }
}

fn swap_image(panel: &NSPanel, image: &NSImage, mt: MainThreadMarker) {
    let sz = unsafe { image.size() };
    unsafe {
        panel.setContentView(Some(&*make_iv(image, mt)));
        panel.setContentSize(sz);
    }
}

// ---- Status item ----

fn make_status_item(mt: MainThreadMarker) -> Retained<objc2_app_kit::NSStatusItem> {
    unsafe {
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(-2.0); // NSSquareStatusItemLength
        if let Some(btn) = item.button(mt) {
            if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str("lizard.fill"),
                None,
            ) {
                img.setTemplate(true);
                btn.setImage(Some(&img));
            }
        }
        let menu = NSMenu::init(NSMenu::alloc(mt));

        // About — routes to NSApp.orderFrontStandardAboutPanel: via responder chain
        let about = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mt),
            &NSString::from_str("About Petit Mates Demo"),
            Some(objc2::sel!(orderFrontStandardAboutPanel:)),
            &NSString::from_str(""),
        );
        menu.addItem(&about);
        menu.addItem(&NSMenuItem::separatorItem(mt));

        let quit = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mt),
            &NSString::from_str("Exit"),
            Some(objc2::sel!(terminate:)),
            &NSString::from_str("q"),
        );
        menu.addItem(&quit);
        item.setMenu(Some(&menu));
        item
    }
}

// ---- Window tracking ----

fn front_win() -> Option<(f64, f64, f64)> {
    let raw =
        unsafe { CGWindowListCopyWindowInfo(OPT_ON_SCREEN | OPT_EXCL_DESKTOP, NULL_WINDOW) };
    if raw.is_null() {
        return None;
    }
    let arr: Retained<NSArray<AnyObject>> =
        unsafe { Retained::from_raw(raw as *mut NSArray<AnyObject>).unwrap() };

    let ws = unsafe { NSWorkspace::sharedWorkspace() };
    let front = unsafe { ws.frontmostApplication() }?;
    let my_pid = std::process::id() as i32;
    let front_pid = unsafe { front.processIdentifier() };
    if front_pid == my_pid {
        return None;
    }

    let k_pid = NSString::from_str("kCGWindowOwnerPID");
    let k_layer = NSString::from_str("kCGWindowLayer");
    let k_bounds = NSString::from_str("kCGWindowBounds");

    let n = unsafe { arr.count() };
    for i in 0..n {
        let obj: Retained<AnyObject> = unsafe { arr.objectAtIndex(i) };
        let dict: &NSDictionary<NSString, AnyObject> =
            unsafe { &*(Retained::as_ptr(&obj) as *const NSDictionary<NSString, AnyObject>) };

        let pid = unsafe { dict.objectForKey(&k_pid) }
            .and_then(|v| as_i32(&v))
            .unwrap_or(-1);
        if pid != front_pid {
            continue;
        }
        let layer = unsafe { dict.objectForKey(&k_layer) }
            .and_then(|v| as_i32(&v))
            .unwrap_or(-1);
        if layer != 0 {
            continue;
        }
        let bobj: Retained<AnyObject> = unsafe { dict.objectForKey(&k_bounds) }?;
        let bd: &NSDictionary<NSString, AnyObject> = unsafe {
            &*(Retained::as_ptr(&bobj) as *const NSDictionary<NSString, AnyObject>)
        };
        let x = dict_f64(bd, "X")?;
        let y = dict_f64(bd, "Y")?;
        let w = dict_f64(bd, "Width")?;
        return Some((x, y, w));
    }
    None
}

fn as_i32(obj: &AnyObject) -> Option<i32> {
    let n: &NSNumber = unsafe { obj.downcast_ref()? };
    Some(unsafe { n.intValue() })
}

fn dict_f64(d: &NSDictionary<NSString, AnyObject>, key: &str) -> Option<f64> {
    let k = NSString::from_str(key);
    let v: Retained<AnyObject> = unsafe { d.objectForKey(&k) }?;
    let n: &NSNumber = unsafe { v.downcast_ref()? };
    Some(unsafe { n.doubleValue() })
}

fn rect_contains(r: NSRect, p: NSPoint) -> bool {
    p.x >= r.origin.x
        && p.x < r.origin.x + r.size.width
        && p.y >= r.origin.y
        && p.y < r.origin.y + r.size.height
}

// ---- Tick ----

fn tick() {
    APP.with(|cell| {
        let mut b = cell.borrow_mut();
        let Some(s) = b.as_mut() else { return };

        let mt = unsafe { MainThreadMarker::new_unchecked() };

        let screen_h = unsafe {
            NSScreen::mainScreen(mt)
                .map(|sc| sc.frame().size.height)
                .unwrap_or(0.0)
        };
        if screen_h == 0.0 {
            return;
        }

        match front_win() {
            Some((wx, wy, ww)) => {
                let menu_h = unsafe {
                    NSApplication::sharedApplication(mt)
                        .mainMenu()
                        .map(|m| m.menuBarHeight())
                        .unwrap_or(24.0)
                };

                let corner_h = unsafe { s.corner_image.size().height };
                let overhang = corner_h - s.corner_anchor_y;
                let new_mode =
                    if wy - menu_h < overhang { Mode::Wall } else { Mode::Corner };

                if new_mode != s.mode {
                    s.mode = new_mode;
                    let img = if new_mode == Mode::Corner {
                        &s.corner_image
                    } else {
                        &s.wall_image
                    };
                    swap_image(&s.panel, img, mt);
                }

                let (anchor_x, py) = match s.mode {
                    Mode::Corner => {
                        let py = screen_h - wy - s.corner_anchor_y;
                        (s.corner_anchor_x, py)
                    }
                    Mode::Wall => {
                        let img_h = unsafe { s.wall_image.size().height };
                        let py = screen_h - wy - img_h;
                        (s.wall_anchor_x, py)
                    }
                };
                let px = wx + ww - anchor_x;

                unsafe {
                    s.panel.setFrameOrigin(NSPoint::new(px, py));
                    if !s.panel.isVisible() {
                        s.panel.orderFront(None);
                    }
                }

                let mouse = unsafe { NSEvent::mouseLocation() };
                let panel_frame = unsafe { s.panel.frame() };
                let over =
                    unsafe { s.panel.isVisible() } && rect_contains(panel_frame, mouse);
                let target = if over { 0.25_f64 } else { 1.0 };
                let cur = unsafe { s.panel.alphaValue() };
                if (cur - target).abs() > 0.01 {
                    unsafe { s.panel.setAlphaValue(target) };
                }
            }
            None => {
                if unsafe { s.panel.isVisible() } {
                    unsafe { s.panel.orderOut(None) };
                }
                s.mode = Mode::Corner;
            }
        }
    });
}

// ---- Entry point ----

pub fn run() {
    let mt = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mt);
    unsafe { app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) };

    // Prevent multiple instances
    if let Some(bid) = unsafe { NSBundle::mainBundle().bundleIdentifier() } {
        let others =
            unsafe { NSRunningApplication::runningApplicationsWithBundleIdentifier(&bid) };
        let my_pid = std::process::id() as i32;
        if unsafe { others.iter().any(|a| a.processIdentifier() != my_pid) } {
            unsafe { app.terminate(None) };
            return;
        }
    }

    let cdir = char_dir().expect("character directory not found");
    let mf = manifest::load(&cdir).expect("manifest.toml not found or invalid");

    let display_w = 150.0_f64;
    let disp_scale = display_w / mf.canonical_width;

    let orig_c = load_img(&cdir, "f-hang-corner.png").expect("f-hang-corner.png not found");
    let orig_w = load_img(&cdir, "f-hang-wall.png").expect("f-hang-wall.png not found");
    let corner_image = scale_img(&orig_c, disp_scale);
    let wall_image = scale_img(&orig_w, disp_scale);

    let sp_c = &mf.sprites["f-hang-corner"];
    let corner_sprite_h = unsafe { orig_c.size().height };
    let corner_anchor_x = sp_c.x.unwrap_or(0.0) * disp_scale;
    let corner_anchor_y = (corner_sprite_h - sp_c.y.unwrap_or(0.0)) * disp_scale;

    let sp_w = &mf.sprites["f-hang-wall"];
    let wall_anchor_x = sp_w.x.unwrap_or(0.0) * disp_scale;

    let panel = make_panel(&corner_image, mt);
    let status_item = make_status_item(mt);

    let blk = RcBlock::new(|_: NonNull<NSTimer>| tick());
    let timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.1, true, &blk)
    };
    unsafe {
        let common: &NSRunLoopMode = &*(NSRunLoopCommonModes as *const NSRunLoopMode);
        NSRunLoop::mainRunLoop().addTimer_forMode(&timer, common);
    }

    APP.with(|cell| {
        *cell.borrow_mut() = Some(State {
            panel,
            corner_image,
            wall_image,
            corner_anchor_x,
            corner_anchor_y,
            wall_anchor_x,
            mode: Mode::Corner,
            _status_item: status_item,
            _timer: timer,
        });
    });

    unsafe { app.run() };
}
