#![allow(dead_code)]

use anyhow::Result;

#[cfg(unix)]
pub fn enable() -> Result<()> {
    // Basic cross-desktop XDG implementation
    let autostart_dir = directories::BaseDirs::new().unwrap().config_dir().join("autostart");
    std::fs::create_dir_all(&autostart_dir).ok();
    let desktop_file = autostart_dir.join("monsoon.desktop");

    let exec_path = std::env::current_exe()?;
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=monsoon\n\
         Exec={} daemon --quiet\n\
         Hidden=false\n\
         NoDisplay=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exec_path.display()
    );
    std::fs::write(&desktop_file, content)?;
    println!("created XDG autostart at {}", desktop_file.display());
    Ok(())
}

#[cfg(unix)]
pub fn disable() -> Result<()> {
    let autostart_dir = directories::BaseDirs::new().unwrap().config_dir().join("autostart");
    let desktop_file = autostart_dir.join("monsoon.desktop");
    if desktop_file.exists() {
        std::fs::remove_file(desktop_file)?;
        println!("removed XDG autostart entry");
    }
    Ok(())
}

#[cfg(windows)]
pub fn enable() -> Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = hkcu.open_subkey_with_flags(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
        KEY_SET_VALUE,
    )?;

    let exec_path = std::env::current_exe()?;
    let cmd = format!("\"{}\" daemon --quiet", exec_path.display());
    run.set_value("monsoon", &cmd)?;
    println!("added monsoon to registry autostart");
    Ok(())
}

#[cfg(windows)]
pub fn disable() -> Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = hkcu.open_subkey_with_flags(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
        KEY_SET_VALUE,
    )?;

    let _ = run.delete_value("monsoon");
    println!("removed monsoon from registry autostart");
    Ok(())
}
