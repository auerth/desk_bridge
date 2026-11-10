//! Which app uses a camera right now, from the Windows privacy bookkeeping
//! (`CapabilityAccessManager\ConsentStore\webcam`: `LastUsedTimeStop` is 0 while in use).
//! It does not say *which* camera, so it is only shown while our virtual camera is in use.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_QWORD, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW,
};
use windows::core::{HSTRING, PWSTR, w};

const ROOT: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\webcam";

fn subkeys(path: &str) -> Vec<String> {
    let mut key = HKEY::default();
    // SAFETY: plain registry calls; the key is closed below.
    unsafe {
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            0,
            KEY_READ,
            &mut key,
        ) != ERROR_SUCCESS
        {
            return vec![];
        }
        let mut out = Vec::new();
        for i in 0.. {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            if RegEnumKeyExW(
                key,
                i,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
                None,
                PWSTR::null(),
                None,
                None,
            ) != ERROR_SUCCESS
            {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        let _ = RegCloseKey(key);
        out
    }
}

fn qword(path: &str, name: windows::core::PCWSTR) -> Option<u64> {
    let mut v = 0u64;
    let mut len = 8u32;
    // SAFETY: out buffer of 8 bytes.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            name,
            RRF_RT_REG_QWORD,
            None,
            Some((&raw mut v).cast()),
            Some(&mut len),
        )
    };
    (r == ERROR_SUCCESS).then_some(v)
}

fn in_use(path: &str) -> Option<u64> {
    let start = qword(path, w!("LastUsedTimeStart"))?;
    let stop = qword(path, w!("LastUsedTimeStop"))?;
    (start > 0 && stop == 0).then_some(start)
}

/// "ms-teams.exe" from `C:#Program Files#...#ms-teams.exe`, "MSTeams" from
/// `MSTeams_8wekyb3d8bbwe`.
fn display_name(key: &str, packaged: bool) -> String {
    if packaged {
        key.split('_').next().unwrap_or(key).to_string()
    } else {
        key.rsplit('#').next().unwrap_or(key).to_string()
    }
}

/// The app that most recently started using a camera and still uses it.
#[must_use]
pub fn camera_app() -> Option<String> {
    let mut best: Option<(u64, String)> = None;
    let mut consider = |path: String, name: String| {
        if let Some(t) = in_use(&path)
            && best.as_ref().is_none_or(|(bt, _)| t > *bt)
        {
            best = Some((t, name));
        }
    };
    for k in subkeys(ROOT) {
        if k != "NonPackaged" {
            consider(format!("{ROOT}\\{k}"), display_name(&k, true));
        }
    }
    let np = format!("{ROOT}\\NonPackaged");
    for k in subkeys(&np) {
        let name = display_name(&k, false);
        if name.eq_ignore_ascii_case("deskbridge-agent.exe") {
            continue;
        }
        consider(format!("{np}\\{k}"), name);
    }
    best.map(|(_, n)| n)
}

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn names() {
        assert_eq!(
            display_name("C:#Program Files#Teams#ms-teams.exe", false),
            "ms-teams.exe"
        );
        assert_eq!(display_name("MSTeams_8wekyb3d8bbwe", true), "MSTeams");
        assert_eq!(display_name("plain", false), "plain");
    }
}
