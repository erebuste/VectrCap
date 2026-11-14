//! Native `RegisterHotKey`/`UnregisterHotKey` backend for `ShortcutManager`.
//!
//! Requests are synchronously marshalled to the tray window owner thread.
//! The fixed message protocol carries validated integers, never pointers.
#![allow(unsafe_code)]
use crate::app::{
    error::AppError,
    shortcuts::{KeyChord, RegistrationId, ShortcutRegistrar},
};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
};

fn modifiers(chord: &KeyChord) -> u32 {
    // MOD_NOREPEAT is the sole source of "dispatch once per press": Windows
    // itself never posts a repeat WM_HOTKEY for a held-down combination.
    let mut mods = MOD_NOREPEAT;
    if chord.ctrl {
        mods |= MOD_CONTROL;
    }
    if chord.alt {
        mods |= MOD_ALT;
    }
    if chord.shift {
        mods |= MOD_SHIFT;
    }
    if chord.win {
        mods |= MOD_WIN;
    }
    mods
}

pub struct Win32HotkeyBackend {
    hwnd: isize,
}

impl Win32HotkeyBackend {
    pub fn new(hwnd: isize) -> Self {
        Win32HotkeyBackend { hwnd }
    }
}

// SAFETY (Send+Sync): `hwnd` is a plain, Copy, pointer-sized identifier. Every
// use of it below is a documented thread-safe Win32 call (RegisterHotKey may
// be called cross-thread; only delivery is bound to the window's own thread).
unsafe impl Send for Win32HotkeyBackend {}
unsafe impl Sync for Win32HotkeyBackend {}

impl ShortcutRegistrar for Win32HotkeyBackend {
    fn register(&self, id: RegistrationId, chord: KeyChord) -> Result<(), AppError> {
        // SAFETY: `hwnd` names a window that outlives the whole app session;
        // id/mods/vk are plain integers with no borrowed/owned resources.
        // Marshal to the window owner: RegisterHotKey rejects foreign-thread HWNDs.

        let ok = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
                self.hwnd as HWND,
                REGISTER_MESSAGE,
                id.0 as usize,
                ((modifiers(&chord) << 16) | chord.key.vk()) as isize,
            )
        };
        if ok == 0 {
            return Err(AppError::InUse(
                "hotkey combination is already in use by Windows or another app",
            ));
        }
        Ok(())
    }

    fn unregister(&self, id: RegistrationId) {
        // SAFETY: same window handle as `register`; a failed unregister (id
        // already gone) is reported by Win32 as a return value, not UB.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
                self.hwnd as HWND,
                UNREGISTER_MESSAGE,
                id.0 as usize,
                0,
            );
        }
    }
}

/// Forwards `WM_HOTKEY` fired on the tray's window to the controller, via a
/// second `SetWindowSubclass` registration on the same `HWND` already used
/// by `appearance::AppearanceWake` (a distinct `uIdSubclass`, so both coexist
/// independently). Detaches on drop, exactly like `AppearanceWake`.
pub struct HotkeyBridge {
    window: HWND,
    client: Option<Box<crate::app::desktop::DesktopClient>>,
}

const REGISTER_MESSAGE: u32 = 0x8000 + 241;
const UNREGISTER_MESSAGE: u32 = 0x8000 + 242;
const SUBCLASS_ID: usize = 0x484F_544B; // "HOTK"

impl HotkeyBridge {
    /// # Safety
    /// `window` must be a valid, currently-open `HWND` owned by the calling
    /// thread for the lifetime of the returned `HotkeyBridge`.
    pub unsafe fn attach(
        window: HWND,
        client: crate::app::desktop::DesktopClient,
    ) -> Result<Self, AppError> {
        let client = Box::new(client);
        unsafe {
            if windows_sys::Win32::UI::Shell::SetWindowSubclass(
                window,
                Some(hotkey_message),
                SUBCLASS_ID,
                (&*client as *const crate::app::desktop::DesktopClient) as usize,
            ) == 0
            {
                return Err(AppError::Persistence("hotkey event subscription failed"));
            }
        }
        Ok(Self {
            window,
            client: Some(client),
        })
    }
}

unsafe extern "system" fn hotkey_message(
    window: HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    _id: usize,
    data: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::{Shell::DefSubclassProc, WindowsAndMessaging::WM_HOTKEY};
    if message == REGISTER_MESSAGE {
        let packed = lparam as u32;
        let mods = packed >> 16;
        let vk = packed & 0xFFFF;
        if wparam == 0
            || wparam > 0xBFFF
            || mods & !(MOD_NOREPEAT | MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_WIN) != 0
            || crate::app::shortcuts::KeyCode::from_vk(vk).is_err()
        {
            return 0;
        }
        return unsafe { RegisterHotKey(window, wparam as i32, mods | MOD_NOREPEAT, vk) } as isize;
    }
    if message == UNREGISTER_MESSAGE {
        return unsafe { UnregisterHotKey(window, wparam as i32) } as isize;
    }
    if message == WM_HOTKEY {
        let id = wparam as i32;
        // No panic may cross the native callback boundary; reference lifetime
        // is bounded by the successful subclass registration and guard below.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            let client = &*(data as *const crate::app::desktop::DesktopClient);
            let _ = client.submit(crate::app::events::AppCommand::NativeHotkeyFired(id));
        }));
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

impl Drop for HotkeyBridge {
    fn drop(&mut self) {
        unsafe {
            let removed = windows_sys::Win32::UI::Shell::RemoveWindowSubclass(
                self.window,
                Some(hotkey_message),
                SUBCLASS_ID,
            ) != 0;
            if !removed && windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(self.window) != 0 {
                // Prefer one leaked client handle over a live native dangling pointer.
                if let Some(client) = self.client.take() {
                    Box::leak(client);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::shortcuts::KeyCode;

    #[test]
    fn modifiers_always_include_no_repeat() {
        let chord = KeyChord {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            key: KeyCode::from_vk(0x41).unwrap(),
        };
        assert_eq!(modifiers(&chord), MOD_NOREPEAT);
    }

    #[test]
    fn modifiers_combine_every_flag() {
        let chord = KeyChord {
            ctrl: true,
            alt: true,
            shift: true,
            win: true,
            key: KeyCode::from_vk(0x41).unwrap(),
        };
        assert_eq!(
            modifiers(&chord),
            MOD_NOREPEAT | MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_WIN
        );
    }
}
