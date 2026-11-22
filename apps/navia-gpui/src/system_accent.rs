//! the desktop's accent colour, for the System accent choice; None where it can't be read.

use futures::channel::mpsc::UnboundedSender;

/// read once, blocking, so the first frame already has it.
pub fn read() -> Option<u32> {
    imp::read()
}

/// sends the colour again whenever the desktop says it changed.
pub fn watch(tx: UnboundedSender<Option<u32>>) {
    imp::watch(tx);
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn the_portal_answer_is_read_through_its_variant() {
        use zbus::zvariant::{OwnedValue, Value};
        let colour = Value::from((0.353f64, 0.357f64, 0.2f64));
        let wrapped = OwnedValue::try_from(Value::Value(Box::new(colour.try_clone().unwrap()))).unwrap();
        assert_eq!(super::imp::from_portal(wrapped), Some(0x5a5b33));
        assert_eq!(super::imp::from_portal(OwnedValue::try_from(colour).unwrap()), Some(0x5a5b33));
        assert_eq!(super::imp::from_portal(OwnedValue::try_from(Value::from((-1.0f64, -1.0f64, -1.0f64))).unwrap()), None);
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::UnboundedSender;
    use zbus::zvariant::{OwnedValue, Value};

    const NAMESPACE: &str = "org.freedesktop.appearance";
    const KEY: &str = "accent-color";

    /// the portal's `(ddd)` in 0..1; any channel outside it means the desktop has none.
    pub(super) fn from_portal(value: OwnedValue) -> Option<u32> {
        // `ReadOne` answers a variant holding the variant, so it's unwrapped as far as it goes
        let mut value = Value::from(value);
        while let Value::Value(inner) = value {
            value = *inner;
        }
        let (r, g, b) = <(f64, f64, f64)>::try_from(value).ok()?;
        let channel = |c: f64| (0.0..=1.0).contains(&c).then(|| (c * 255.0).round() as u32);
        Some((channel(r)? << 16) | (channel(g)? << 8) | channel(b)?)
    }

    /// Plasma's own setting, for desktops whose portal has no accent.
    fn from_kdeglobals() -> Option<u32> {
        let dir = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
        let text = std::fs::read_to_string(dir.join("kdeglobals")).ok()?;
        let mut general = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                general = line == "[General]";
            } else if general && let Some(value) = line.strip_prefix("AccentColor=") {
                let parts: Vec<u32> = value.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                if let [r, g, b, ..] = parts[..] {
                    return Some((r.min(255) << 16) | (g.min(255) << 8) | b.min(255));
                }
            }
        }
        None
    }

    fn from_portal_now() -> Option<u32> {
        let connection = zbus::blocking::Connection::session().ok()?;
        let proxy = zbus::blocking::Proxy::new(&connection, "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Settings").ok()?;
        from_portal(proxy.call::<_, _, OwnedValue>("ReadOne", &(NAMESPACE, KEY)).ok()?)
    }

    pub fn read() -> Option<u32> {
        from_portal_now().or_else(from_kdeglobals)
    }

    pub fn watch(tx: UnboundedSender<Option<u32>>) {
        crate::chrome::watch_portal_setting("navia-accent-setting", NAMESPACE, KEY, move |value| {
            let _ = tx.unbounded_send(from_portal(value).or_else(from_kdeglobals));
        });
    }
}

#[cfg(windows)]
mod imp {
    use super::UnboundedSender;

    const DWM: &str = "Software\\Microsoft\\Windows\\DWM";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// DWM's `AccentColor`, stored as 0xAABBGGRR.
    pub fn read() -> Option<u32> {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
        let (mut value, mut size) = (0u32, 4u32);
        let read = unsafe { RegGetValueW(HKEY_CURRENT_USER, wide(DWM).as_ptr(), wide("AccentColor").as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut value as *mut u32).cast(), &mut size) };
        (read == ERROR_SUCCESS).then(|| ((value & 0xff) << 16) | (value & 0xff00) | ((value >> 16) & 0xff))
    }

    pub fn watch(tx: UnboundedSender<Option<u32>>) {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, REG_NOTIFY_CHANGE_LAST_SET, RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW};
        let _ = std::thread::Builder::new().name("navia-accent-setting".into()).spawn(move || {
            let mut key: HKEY = std::ptr::null_mut();
            if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide(DWM).as_ptr(), 0, KEY_NOTIFY, &mut key) } != ERROR_SUCCESS {
                return;
            }
            // blocks until a value under the key changes.
            while unsafe { RegNotifyChangeKeyValue(key, 0, REG_NOTIFY_CHANGE_LAST_SET, std::ptr::null_mut(), 0) } == ERROR_SUCCESS {
                if tx.unbounded_send(read()).is_err() {
                    break;
                }
            }
            unsafe { RegCloseKey(key) };
        });
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::UnboundedSender;

    pub fn read() -> Option<u32> {
        None
    }

    pub fn watch(_tx: UnboundedSender<Option<u32>>) {}
}
