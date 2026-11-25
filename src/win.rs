//! The few direct Windows calls: one registry read and the working-set trim.

use std::ptr::null_mut;
use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows_sys::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// A user environment variable, read from where Windows keeps it. A process started from another
/// app does not always inherit these, so the process environment alone is not enough.
pub fn user_env(name: &str) -> Option<String> {
    let key: Vec<u16> = "Environment".encode_utf16().chain(Some(0)).collect();
    let value: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    let mut size: u32 = 0;
    // SAFETY: both strings are NUL-terminated and outlive the calls; the buffer is sized by the
    // first call and its byte length is passed to the second.
    unsafe {
        if RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), flags, null_mut(), null_mut(), &mut size) != 0 {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2) + 1];
        let mut bytes = (buf.len() * 2) as u32;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            flags,
            null_mut(),
            buf.as_mut_ptr().cast(),
            &mut bytes,
        );
        if status != 0 {
            buf.fill(0);
            return None;
        }
        buf.truncate(bytes as usize / 2);
        while buf.last() == Some(&0) {
            buf.pop();
        }
        let out = String::from_utf16(&buf).ok();
        buf.fill(0);
        out.filter(|s| !s.trim().is_empty())
    }
}

/// Hands memory the app is not touching back to Windows. Called when the game goes away.
pub fn trim_working_set() {
    // SAFETY: the pseudo-handle for the current process is always valid.
    unsafe {
        K32EmptyWorkingSet(GetCurrentProcess());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_variable_reads_as_none() {
        assert!(user_env("SIGHTLINE_NO_SUCH_VARIABLE_4f2a").is_none());
    }

    #[test]
    fn a_standard_variable_can_be_read() {
        // TEMP is a user variable on every Windows install; only its presence is checked.
        assert!(user_env("TEMP").is_some());
    }
}
