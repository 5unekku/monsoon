#![allow(dead_code)]

use anyhow::Result;

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(unix)]
pub fn is_alive(pid: i32) -> bool {
    unsafe { kill(pid, 0) == 0 }
}

#[cfg(unix)]
pub fn terminate(pid: i32) -> Result<()> {
    if unsafe { kill(pid, 15) } != 0 {
        anyhow::bail!("kill {}: {}", pid, std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
pub fn is_alive(pid: i32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject};
    
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if handle == std::ptr::null_mut() {
            return false;
        }
        let wait_res = WaitForSingleObject(handle, 0);
        CloseHandle(handle);
        wait_res == WAIT_TIMEOUT
    }
}

#[cfg(windows)]
pub fn terminate(pid: i32) -> Result<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
        if handle == std::ptr::null_mut() {
            anyhow::bail!("failed to open process {}", pid);
        }
        let res = TerminateProcess(handle, 1);
        CloseHandle(handle);
        if res == 0 {
            anyhow::bail!("failed to terminate process {}", pid);
        }
    }
    Ok(())
}
