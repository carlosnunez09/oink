//! Whether this computer is running on battery, so background checks can slow
//! down on a laptop's battery and stay fast on AC power or a desktop.
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one power-source reading is reused.
const CACHE: Duration = Duration::from_secs(30);
/// Background network/cable check interval on AC power (or a desktop).
const ON_AC: Duration = Duration::from_secs(1);
/// Background network/cable check interval on battery.
const ON_BATTERY: Duration = Duration::from_secs(3);

static LAST: Mutex<Option<(Instant, bool)>> = Mutex::new(None);

/// True only when the system reports it is running from its battery.
/// Unknown or unsupported power sources count as AC.
pub(crate) fn on_battery() -> bool {
    let now = Instant::now();
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, value)) = *last
        && now.saturating_duration_since(at) < CACHE
    {
        return value;
    }
    let value = platform::on_battery();
    *last = Some((now, value));
    value
}

/// Interval for the cable monitor and local-address checks.
pub(crate) fn check_interval() -> Duration {
    interval_for(on_battery())
}

fn interval_for(battery: bool) -> Duration {
    if battery { ON_BATTERY } else { ON_AC }
}

#[cfg(windows)]
mod platform {
    #[repr(C)]
    #[derive(Default)]
    struct SystemPowerStatus {
        ac_line_status: u8,
        battery_flag: u8,
        battery_life_percent: u8,
        system_status_flag: u8,
        battery_life_time: u32,
        battery_full_life_time: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemPowerStatus(status: *mut SystemPowerStatus) -> i32;
    }
    pub(super) fn on_battery() -> bool {
        let mut status = SystemPowerStatus::default();
        // SAFETY: `status` is a live, correctly laid out SYSTEM_POWER_STATUS.
        let ok = unsafe { GetSystemPowerStatus(&mut status) } != 0;
        // ACLineStatus 0 = offline (battery); 1 = online; 255 = unknown.
        ok && status.ac_line_status == 0
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CStr, c_char, c_void};
    type CFTypeRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
        fn IOPSGetProvidingPowerSourceType(snapshot: CFTypeRef) -> CFTypeRef;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFStringGetCString(string: CFTypeRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    }
    pub(super) fn on_battery() -> bool {
        // SAFETY: Copy rule; released below. Null means no power information.
        let snapshot = unsafe { IOPSCopyPowerSourcesInfo() };
        if snapshot.is_null() {
            return false;
        }
        // SAFETY: valid snapshot; the returned CFString follows the Get rule
        // (owned by the snapshot), so it is only read before the release.
        let kind = unsafe { IOPSGetProvidingPowerSourceType(snapshot) };
        let mut buffer = [0 as c_char; 64];
        // SAFETY: non-null CFString and a writable 64-byte buffer.
        let read = !kind.is_null()
            && unsafe { CFStringGetCString(kind, buffer.as_mut_ptr(), buffer.len() as isize, UTF8) } != 0;
        // SAFETY: CFStringGetCString succeeded, so the buffer is NUL-terminated.
        let battery = read && unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_bytes() == b"Battery Power";
        // SAFETY: balances the Copy above; `kind` is not used afterwards.
        unsafe { CFRelease(snapshot) };
        battery
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    pub(super) fn on_battery() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn battery_slows_checks_to_three_seconds() {
        assert_eq!(interval_for(false), Duration::from_secs(1));
        assert_eq!(interval_for(true), Duration::from_secs(3));
    }
}
