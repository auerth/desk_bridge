//! SendTo shortcut ("Desk Bridge – Laptop senden") created through `IShellLinkW`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{HSTRING, Interface};

use crate::brand::{PRODUCT_NAME, SENDTO_SHORTCUT_NAME};

/// Full path of the `.lnk` inside `sendto_dir`.
#[must_use]
pub fn shortcut_path(sendto_dir: &Path) -> PathBuf {
    sendto_dir.join(format!("{SENDTO_SHORTCUT_NAME}.lnk"))
}

/// Create (or overwrite) a shortcut that runs `deskbridge-agent.exe send <files...>`.
/// Windows appends the selected paths to the arguments.
pub fn create(sendto_dir: &Path, agent_exe: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(sendto_dir).context("creating SendTo directory")?;
    let lnk = shortcut_path(sendto_dir);
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }
        .context("CoCreateInstance(ShellLink)")?;
    unsafe {
        link.SetPath(&HSTRING::from(agent_exe.as_os_str()))?;
        link.SetArguments(&HSTRING::from("send"))?;
        link.SetDescription(&HSTRING::from(format!(
            "Send the selected files to the paired device with {PRODUCT_NAME}"
        )))?;
        if let Some(dir) = agent_exe.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        link.SetIconLocation(&HSTRING::from(agent_exe.as_os_str()), 0)?;
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true)
            .context("IPersistFile::Save")?;
    }
    Ok(lnk)
}

/// Remove a shortcut; a missing file is not an error.
pub fn remove(lnk: &Path) -> Result<()> {
    match std::fs::remove_file(lnk) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", lnk.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_name_is_localised_and_has_lnk_extension() {
        let p = shortcut_path(Path::new(r"C:\SendTo"));
        assert_eq!(p.extension().unwrap(), "lnk");
        assert!(
            p.file_stem()
                .unwrap()
                .to_string_lossy()
                .contains("Laptop senden")
        );
    }
}
