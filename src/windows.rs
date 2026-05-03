#![cfg(target_os = "windows")]
#![allow(non_snake_case)]

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem;
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use image::GenericImageView;

use crate::manifest;

// ---- Constants ----

const WM_TRAY: u32 = WM_APP + 1;
const IDM_ABOUT: usize = 1;
const IDM_EXIT: usize = 2;
const TIMER_TICK: usize = 1;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---- Theme detection ----

/// Returns true when the Windows taskbar is in dark mode (needs light/white icon).
fn is_dark_mode() -> bool {
    unsafe {
        let subkey = to_wide(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
        );
        let value = to_wide("SystemUsesLightTheme");
        let mut hkey: HKEY = ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey,
        ) != 0 {
            return false; // assume light mode on failure
        }
        let mut data: u32 = 1; // 1 = light mode (default)
        let mut size = mem::size_of::<u32>() as u32;
        RegQueryValueExW(
            hkey,
            value.as_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut data as *mut u32 as *mut u8,
            &mut size,
        );
        RegCloseKey(hkey);
        data == 0 // 0 = dark mode → need white icon
    }
}

// ---- Sprite (pre-multiplied BGRA for UpdateLayeredWindow) ----

struct Sprite {
    bgra: Vec<u8>,
    width: i32,
    height: i32,
}

impl Sprite {
    fn load(char_dir: &Path, name: &str, scale: f64) -> Self {
        let path = char_dir.join("sprite").join(name);
        let img = image::open(&path)
            .unwrap_or_else(|_| panic!("{name} not found"));
        let (ow, oh) = img.dimensions();
        let nw = ((ow as f64 * scale).round() as u32).max(1);
        let nh = ((oh as f64 * scale).round() as u32).max(1);
        let img = img
            .resize_exact(nw, nh, image::imageops::FilterType::Triangle)
            .to_rgba8();
        // RGBA straight alpha → BGRA pre-multiplied alpha
        let bgra: Vec<u8> = img
            .pixels()
            .flat_map(|p| {
                let a = p[3] as u32;
                [
                    (p[2] as u32 * a / 255) as u8, // B
                    (p[1] as u32 * a / 255) as u8, // G
                    (p[0] as u32 * a / 255) as u8, // R
                    p[3],                           // A (unchanged)
                ]
            })
            .collect();
        Sprite { bgra, width: nw as i32, height: nh as i32 }
    }
}

// ---- App state ----

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Corner,
    Wall,
}

struct State {
    hwnd: HWND,
    corner: Sprite,
    wall: Sprite,
    corner_anchor_x: f64,
    corner_anchor_y: f64, // top-left origin (Windows native)
    wall_anchor_x: f64,
    mode: Mode,
    visible: bool,
    hovered: bool,
}

thread_local! {
    static APP: RefCell<Option<State>> = RefCell::new(None);
}

// ---- Layered window content update ----
//
// Uploads `bgra` (pre-multiplied BGRA) to a DIB and calls UpdateLayeredWindow.
// `x`,`y` are screen-space top-left of the window after this call.

unsafe fn set_layered_content(
    hwnd: HWND,
    bgra: &[u8],
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    alpha: u8,
) {
    let hdc_screen = GetDC(ptr::null_mut());
    let hdc_mem = CreateCompatibleDC(hdc_screen);

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 }],
    };

    let mut bits: *mut c_void = ptr::null_mut();
    let hbmp = CreateDIBSection(hdc_mem, &bmi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
    ptr::copy_nonoverlapping(bgra.as_ptr(), bits as *mut u8, bgra.len());

    let old = SelectObject(hdc_mem, hbmp);
    let pt_dst = POINT { x, y };
    let size = SIZE { cx: width, cy: height };
    let pt_src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    UpdateLayeredWindow(
        hwnd,
        hdc_screen,
        &pt_dst,
        &size,
        hdc_mem,
        &pt_src,
        0,
        &blend,
        ULW_ALPHA,
    );

    SelectObject(hdc_mem, old);
    DeleteObject(hbmp);
    DeleteDC(hdc_mem);
    ReleaseDC(ptr::null_mut(), hdc_screen);
}

// ---- Front window tracking ----

fn front_win(my_hwnd: HWND) -> Option<(f64, f64, f64)> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() || hwnd == my_hwnd {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == GetCurrentProcessId() {
            return None;
        }
        if IsIconic(hwnd) != 0 || IsWindowVisible(hwnd) == 0 {
            return None;
        }
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        if GetWindowRect(hwnd, &mut r) == 0 {
            return None;
        }
        let w = (r.right - r.left) as f64;
        if w <= 0.0 {
            return None;
        }
        Some((r.left as f64, r.top as f64, w))
    }
}

// ---- Tick (called every 100 ms from WM_TIMER) ----

fn tick(hwnd: HWND) {
    APP.with(|cell| {
        let mut b = cell.borrow_mut();
        let Some(s) = b.as_mut() else { return };

        match front_win(s.hwnd) {
            Some((wx, wy, ww)) => {
                // Switch to wall mode when the window top is too close to screen top.
                // On Windows y=0 is the screen top; the anchor_y is from the sprite top.
                let new_mode =
                    if wy < s.corner_anchor_y { Mode::Wall } else { Mode::Corner };
                let mode_changed = new_mode != s.mode;
                s.mode = new_mode;

                let (anchor_x, anchor_y, w, h) = match s.mode {
                    Mode::Corner => (
                        s.corner_anchor_x,
                        s.corner_anchor_y,
                        s.corner.width,
                        s.corner.height,
                    ),
                    Mode::Wall => (s.wall_anchor_x, 0.0, s.wall.width, s.wall.height),
                };

                // Sprite top-left: anchor point aligns with window top-right corner.
                let sx = (wx + ww - anchor_x) as i32;
                let sy = (wy - anchor_y) as i32;

                // Hover: check whether the cursor is within the sprite's bounding box.
                // (WS_EX_TRANSPARENT passes mouse events through, but GetCursorPos still works.)
                let new_hovered = unsafe {
                    let mut pt = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut pt) != 0
                        && pt.x >= sx && pt.x < sx + w
                        && pt.y >= sy && pt.y < sy + h
                };
                let hover_changed = new_hovered != s.hovered;
                s.hovered = new_hovered;
                // Match Mac behaviour: hovered → nearly invisible (25 %), normal → opaque.
                let alpha: u8 = if s.hovered { 64 } else { 255 };

                unsafe {
                    if mode_changed || !s.visible || hover_changed {
                        let bgra =
                            if s.mode == Mode::Corner { &s.corner.bgra } else { &s.wall.bgra };
                        set_layered_content(hwnd, bgra, w, h, sx, sy, alpha);
                        if !s.visible {
                            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                            s.visible = true;
                        }
                    } else {
                        // Position only — no content change.
                        SetWindowPos(
                            hwnd,
                            HWND_TOPMOST,
                            sx,
                            sy,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );
                    }
                }
            }
            None => {
                if s.visible {
                    unsafe { ShowWindow(hwnd, SW_HIDE); }
                    s.visible = false;
                }
                s.hovered = false;
            }
        }
    });
}

// ---- Window procedure ----

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    unsafe { match msg {
        WM_TIMER if wp == TIMER_TICK => {
            tick(hwnd);
            0
        }
        WM_TRAY => {
            if (lp as u32) & 0xFFFF == WM_RBUTTONUP {
                let menu = CreatePopupMenu();
                let about_str = to_wide("About Petit Mates Demo");
                let exit_str = to_wide("Exit");
                AppendMenuW(menu, MF_STRING, IDM_ABOUT, about_str.as_ptr());
                AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
                AppendMenuW(menu, MF_STRING, IDM_EXIT, exit_str.as_ptr());
                let mut pt = POINT { x: 0, y: 0 };
                GetCursorPos(&mut pt);
                SetForegroundWindow(hwnd);
                TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, ptr::null());
                DestroyMenu(menu);
            }
            0
        }
        WM_COMMAND if (wp & 0xFFFF) == IDM_ABOUT => {
            let text = to_wide("Petit Mates Demo\r\nVersion 0.1.0");
            let title = to_wide("About Petit Mates Demo");
            MessageBoxW(ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONINFORMATION);
            0
        }
        WM_COMMAND if (wp & 0xFFFF) == IDM_EXIT => {
            PostQuitMessage(0);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        WM_SETTINGCHANGE => {
            // Theme may have changed; refresh tray icon colour.
            update_tray_icon(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    } }
}

// ---- System tray ----

fn add_tray_icon(hwnd: HWND, hinstance: HINSTANCE) {
    unsafe {
        let tip = to_wide("Petit Mates");
        let mut nid: NOTIFYICONDATAW = mem::zeroed();
        nid.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        // Choose icon based on current theme:
        //   dark mode (dark taskbar) → ID 3 white silhouette
        //   light mode               → ID 2 dark silhouette
        let icon_id: usize = if is_dark_mode() { 3 } else { 2 };
        let hicon = LoadImageW(
            hinstance,
            icon_id as *const u16,
            IMAGE_ICON,
            16, 16,
            LR_SHARED,
        ) as HICON;
        nid.hIcon = if !hicon.is_null() {
            hicon
        } else {
            LoadIconW(ptr::null_mut(), IDI_APPLICATION)
        };
        let n = tip.len().min(nid.szTip.len());
        nid.szTip[..n].copy_from_slice(&tip[..n]);
        Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

/// Refresh only the tray icon (called on WM_SETTINGCHANGE when theme changes).
fn update_tray_icon(hwnd: HWND) {
    unsafe {
        let hinstance = GetModuleHandleW(ptr::null());
        let icon_id: usize = if is_dark_mode() { 3 } else { 2 };
        let hicon = LoadImageW(
            hinstance,
            icon_id as *const u16,
            IMAGE_ICON,
            16, 16,
            LR_SHARED,
        ) as HICON;
        if hicon.is_null() { return; }
        let mut nid: NOTIFYICONDATAW = mem::zeroed();
        nid.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_ICON;
        nid.hIcon = hicon;
        Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn remove_tray_icon(hwnd: HWND) {
    unsafe {
        let mut nid: NOTIFYICONDATAW = mem::zeroed();
        nid.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

// ---- char_dir ----

pub fn char_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;

    // Distribution: assets/ next to the .exe
    let dist = exe_dir.join("assets/bearded_dragon");
    if dist.exists() {
        return Some(dist);
    }

    // Dev cross-compiled: exe lives at target/<triple>/release/ (5 levels to project root)
    // or target/release/ (4 levels). Try both.
    for rel in &[
        "../../../../assets/bearded_dragon",
        "../../../../../assets/bearded_dragon",
    ] {
        if let Ok(p) = exe_dir.join(rel).canonicalize() {
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

// ---- Entry point ----

pub fn run() {
    unsafe {
        let hinstance = GetModuleHandleW(ptr::null());
        let class_name = to_wide("PetitMatesOverlay");

        let wc = WNDCLASSEXW {
            cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: LoadIconW(hinstance, 1usize as *const u16),
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: LoadIconW(hinstance, 1usize as *const u16),
        };
        RegisterClassExW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE
                | WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            ptr::null(),
            WS_POPUP,
            0, 0, 1, 1,
            ptr::null_mut(), ptr::null_mut(), hinstance, ptr::null(),
        );
        assert!(!hwnd.is_null(), "CreateWindowExW failed");

        // Load assets
        let cdir = char_dir().expect("character directory not found");
        let mf = manifest::load(&cdir).expect("manifest.toml not found or invalid");
        let disp_scale = 150.0 / mf.canonical_width;

        let corner = Sprite::load(&cdir, "f-hang-corner.png", disp_scale);
        let wall = Sprite::load(&cdir, "f-hang-wall.png", disp_scale);

        let sp_c = &mf.sprites["f-hang-corner"];
        let corner_anchor_x = sp_c.x.unwrap_or(0.0) * disp_scale;
        let corner_anchor_y = sp_c.y.unwrap_or(0.0) * disp_scale; // top-left origin on Windows

        let sp_w = &mf.sprites["f-hang-wall"];
        let wall_anchor_x = sp_w.x.unwrap_or(0.0) * disp_scale;

        // Initial content upload (positions window off-screen until first tick)
        set_layered_content(hwnd, &corner.bgra, corner.width, corner.height, -4096, -4096, 255);

        APP.with(|cell| {
            *cell.borrow_mut() = Some(State {
                hwnd,
                corner,
                wall,
                corner_anchor_x,
                corner_anchor_y,
                wall_anchor_x,
                mode: Mode::Corner,
                visible: false,
                hovered: false,
            });
        });

        add_tray_icon(hwnd, hinstance);
        SetTimer(hwnd, TIMER_TICK, 100, None);

        // Message loop
        let mut msg: MSG = mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        remove_tray_icon(hwnd);
    }
}
