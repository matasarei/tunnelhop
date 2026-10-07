//! Safe wrappers over the few Win32 calls the service needs. All `unsafe` lives here.

use std::ffi::{OsStr, OsString, c_void};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, EqualSid, GetSecurityDescriptorDacl, GetTokenInformation,
    IsWellKnownSid, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, FILE_ATTRIBUTE_REPARSE_POINT, GetFileVersionInfoSizeW, GetFileVersionInfoW,
    VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_ProgramData, FOLDERID_ProgramFiles, SHGetKnownFolderPath,
};
use windows_sys::core::GUID;
use zeroize::{Zeroize, Zeroizing};

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// # Safety
/// `p` must point at a NUL-terminated UTF-16 string.
unsafe fn from_wide_ptr(p: *const u16) -> OsString {
    let mut len = 0;
    // SAFETY: the caller guarantees a terminator, so every offset up to it is in bounds.
    unsafe {
        while *p.add(len) != 0 {
            len += 1;
        }
        OsString::from_wide(std::slice::from_raw_parts(p, len))
    }
}

/// Frees a buffer the system allocated with `LocalAlloc`.
struct LocalBox(*mut c_void);

impl Drop for LocalBox {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from an API documented to allocate with LocalAlloc.
            unsafe { LocalFree(self.0) };
        }
    }
}

fn win32_result(code: u32) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

fn known_folder(id: &GUID) -> io::Result<PathBuf> {
    let mut path: *mut u16 = ptr::null_mut();
    // SAFETY: valid GUID and out pointer; the buffer is freed with CoTaskMemFree as documented,
    // even on failure.
    let hr = unsafe { SHGetKnownFolderPath(id, 0, ptr::null_mut(), &mut path) };
    let result = if hr >= 0 {
        // SAFETY: on success `path` is a NUL-terminated string.
        Ok(PathBuf::from(unsafe { from_wide_ptr(path) }))
    } else {
        Err(io::Error::from_raw_os_error(hr))
    };
    unsafe { CoTaskMemFree(path as *const c_void) };
    result
}

/// `%ProgramData%`, from the shell rather than the environment.
pub fn program_data() -> io::Result<PathBuf> {
    known_folder(&FOLDERID_ProgramData)
}

/// `%ProgramFiles%` (the 64-bit one for a 64-bit process).
pub fn program_files() -> io::Result<PathBuf> {
    known_folder(&FOLDERID_ProgramFiles)
}

/// A security descriptor parsed from SDDL.
pub struct SecurityDescriptor(LocalBox);

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: NUL-terminated input and a valid out pointer.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(sddl).as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(SecurityDescriptor(LocalBox(sd)))
    }

    fn dacl(&self) -> io::Result<*mut ACL> {
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl: *mut ACL = ptr::null_mut();
        // SAFETY: self.0 is a valid descriptor for as long as self lives.
        let ok =
            unsafe { GetSecurityDescriptorDacl(self.0.0, &mut present, &mut dacl, &mut defaulted) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if present == 0 || dacl.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "security descriptor has no DACL",
            ));
        }
        Ok(dacl)
    }
}

/// Create a directory that carries `sddl` from the first instant, so it is never briefly
/// open to whatever the parent grants. `Ok(false)` when it already exists.
pub fn create_dir_with_sddl(path: &Path, sddl: &str) -> io::Result<bool> {
    let sd = SecurityDescriptor::from_sddl(sddl)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0.0,
        bInheritHandle: 0,
    };
    // SAFETY: NUL-terminated path; the descriptor outlives the call.
    if unsafe { CreateDirectoryW(wide(path).as_ptr(), &attributes) } != 0 {
        return Ok(true);
    }
    // SAFETY: no call in between could have changed the thread's last error.
    match unsafe { GetLastError() } {
        ERROR_ALREADY_EXISTS => Ok(false),
        code => Err(io::Error::from_raw_os_error(code as i32)),
    }
}

/// Replace the DACL of `path` with the one in `sddl`, protected from inheritance. Inheritable
/// entries propagate to the existing contents.
pub fn set_dacl(path: &Path, sddl: &str) -> io::Result<()> {
    let sd = SecurityDescriptor::from_sddl(sddl)?;
    let dacl = sd.dacl()?;
    // SAFETY: NUL-terminated path; the DACL belongs to `sd`, which outlives the call.
    win32_result(unsafe {
        SetNamedSecurityInfoW(
            wide(path).as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            dacl,
            ptr::null(),
        )
    })
}

/// The DACL of `path` in SDDL, as `icacls` would describe it.
pub fn dacl_sddl(path: &Path) -> io::Result<String> {
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: NUL-terminated path and valid out pointers; `sd` is freed by LocalBox.
    win32_result(unsafe {
        GetNamedSecurityInfoW(
            wide(path).as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut sd,
        )
    })?;
    let sd = LocalBox(sd);
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: `sd` is a valid descriptor; `text` is freed by LocalBox.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd.0,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let text = LocalBox(text.cast());
    // SAFETY: on success the output is a NUL-terminated string.
    Ok(unsafe { from_wide_ptr(text.0.cast()) }
        .to_string_lossy()
        .into_owned())
}

/// Whether `path` is a symlink, junction or other reparse point (not followed).
pub fn is_reparse_point(path: &Path) -> io::Result<bool> {
    Ok(std::fs::symlink_metadata(path)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// Whether `path` is owned by SYSTEM, the Administrators group, or the account this process
/// runs as. Anything else may have been planted by another user.
pub fn has_trusted_owner(path: &Path) -> io::Result<bool> {
    let mut owner: PSID = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: NUL-terminated path and valid out pointers; `owner` points into `sd`.
    win32_result(unsafe {
        GetNamedSecurityInfoW(
            wide(path).as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut sd,
        )
    })?;
    let _sd = LocalBox(sd);
    // SAFETY: `owner` is a valid SID while `_sd` lives.
    let well_known = unsafe {
        IsWellKnownSid(owner, WinLocalSystemSid) != 0
            || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
    };
    Ok(well_known || is_current_user(owner)?)
}

fn is_current_user(sid: PSID) -> io::Result<bool> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: the pseudo-handle needs no closing; `token` is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut len = 0u32;
    // SAFETY: the first call only reports the size; the second fills a buffer of that size,
    // aligned for TOKEN_USER by using u64 storage.
    let result = unsafe {
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut len);
        let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
        if GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), len, &mut len) == 0 {
            Err(io::Error::last_os_error())
        } else {
            let user = &*(buffer.as_ptr() as *const TOKEN_USER);
            Ok(EqualSid(user.User.Sid, sid) != 0)
        }
    };
    unsafe { CloseHandle(token) };
    result
}

/// The fixed file version (`major.minor.build`) from an executable's version resource.
pub fn file_version(path: &Path) -> Option<(u32, u32, u32)> {
    let path = wide(path);
    let mut ignored = 0u32;
    // SAFETY: NUL-terminated path; the buffer has the size the first call reported, and the
    // fixed info points into it while it lives.
    unsafe {
        let size = GetFileVersionInfoSizeW(path.as_ptr(), &mut ignored);
        if size == 0 {
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        if GetFileVersionInfoW(path.as_ptr(), 0, size, buffer.as_mut_ptr().cast()) == 0 {
            return None;
        }
        let mut info: *mut c_void = ptr::null_mut();
        let mut len = 0u32;
        let root = wide("\\");
        if VerQueryValueW(buffer.as_ptr().cast(), root.as_ptr(), &mut info, &mut len) == 0
            || info.is_null()
            || (len as usize) < size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        let fixed = &*(info as *const VS_FIXEDFILEINFO);
        if fixed.dwSignature != 0xFEEF_04BD {
            return None;
        }
        Some((
            fixed.dwFileVersionMS >> 16,
            fixed.dwFileVersionMS & 0xFFFF,
            fixed.dwFileVersionLS >> 16,
        ))
    }
}

/// A `REG_SZ` value under `HKEY_LOCAL_MACHINE`, `None` if it is absent or not a string.
pub fn read_hklm_string(subkey: &str, value: &str) -> Option<String> {
    let (subkey, value) = (wide(subkey), wide(value));
    let mut size = 0u32;
    // SAFETY: NUL-terminated names; the second call writes at most `size` bytes into a buffer
    // at least that large.
    unsafe {
        let probe = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut size,
        );
        if probe != 0 {
            return None;
        }
        let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
        let read = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        );
        if read != 0 {
            return None;
        }
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..end]))
    }
}

/// Extra input mixed into every DPAPI blob, so blobs made by other software for the same
/// account do not open as ours and ours do not open elsewhere without it.
const DPAPI_ENTROPY: &[u8] = b"TunnelHop credentials v1";

fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    }
}

/// Encrypt with DPAPI for the account this process runs as (LocalSystem in the service), so
/// only that account on this machine can decrypt.
pub fn dpapi_protect(plain: &[u8]) -> io::Result<Vec<u8>> {
    let (input, entropy) = (blob(plain), blob(DPAPI_ENTROPY));
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // SAFETY: the input blobs borrow live slices that the call only reads; the output buffer is
    // LocalAlloc'd by the system and freed by LocalBox.
    let ok = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            &entropy,
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let owned = LocalBox(output.pbData.cast());
    // SAFETY: on success the output holds cbData bytes.
    Ok(
        unsafe { std::slice::from_raw_parts(owned.0 as *const u8, output.cbData as usize) }
            .to_vec(),
    )
}

pub fn dpapi_unprotect(sealed: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    let (input, entropy) = (blob(sealed), blob(DPAPI_ENTROPY));
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // SAFETY: as in dpapi_protect.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            &entropy,
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let owned = LocalBox(output.pbData.cast());
    // SAFETY: on success the output holds cbData writable bytes; they are copied, then wiped
    // before the system buffer is freed.
    let plain = unsafe {
        let bytes = std::slice::from_raw_parts_mut(owned.0 as *mut u8, output.cbData as usize);
        let copy = Zeroizing::new(bytes.to_vec());
        bytes.zeroize();
        copy
    };
    Ok(plain)
}

/// The LUID of the network interface called `alias` (a WireGuard tunnel's interface carries the
/// tunnel's name).
pub fn interface_luid(alias: &str) -> io::Result<u64> {
    use windows_sys::Win32::NetworkManagement::IpHelper::ConvertInterfaceAliasToLuid;
    use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;

    let mut luid = NET_LUID_LH { Value: 0 };
    // SAFETY: NUL-terminated alias and a valid out pointer.
    win32_result(unsafe { ConvertInterfaceAliasToLuid(wide(alias).as_ptr(), &mut luid) })?;
    // SAFETY: every bit pattern of the union is a valid u64.
    Ok(unsafe { luid.Value })
}

/// The names of every Win32 service whose name starts with `prefix`, compared without regard
/// to case as the SCM does.
pub fn services_with_prefix(prefix: &str) -> io::Result<Vec<String>> {
    use windows_sys::Win32::Foundation::ERROR_MORE_DATA;
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, ENUM_SERVICE_STATUS_PROCESSW, EnumServicesStatusExW, OpenSCManagerW,
        SC_ENUM_PROCESS_INFO, SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_STATE_ALL,
        SERVICE_WIN32,
    };

    // SAFETY: local machine, default database.
    let manager = unsafe {
        OpenSCManagerW(
            ptr::null(),
            ptr::null(),
            SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE,
        )
    };
    if manager.is_null() {
        return Err(io::Error::last_os_error());
    }
    let prefix = prefix.to_lowercase();
    let mut names = Vec::new();
    let mut resume = 0u32;
    // u64 storage keeps the buffer aligned for the structures written into it.
    let mut buffer = vec![0u64; 8 * 1024];
    let result = loop {
        let (mut needed, mut count) = (0u32, 0u32);
        // SAFETY: the buffer is as large as the size passed; the entries it receives, and the
        // strings they point to, live inside it until the next call.
        let ok = unsafe {
            EnumServicesStatusExW(
                manager,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * 8) as u32,
                &mut needed,
                &mut count,
                &mut resume,
                ptr::null(),
            )
        };
        let more = ok == 0 && unsafe { GetLastError() } == ERROR_MORE_DATA;
        if ok == 0 && !more {
            break Err(io::Error::last_os_error());
        }
        // SAFETY: the call wrote `count` entries at the start of the buffer.
        let entries = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW,
                count as usize,
            )
        };
        for entry in entries {
            // SAFETY: each name is a NUL-terminated string inside the buffer.
            let name = unsafe { from_wide_ptr(entry.lpServiceName) }
                .to_string_lossy()
                .into_owned();
            if name.to_lowercase().starts_with(&prefix) {
                names.push(name);
            }
        }
        if !more {
            break Ok(names);
        }
        if count == 0 {
            // Not even one entry fitted: grow to what the SCM asked for.
            buffer.resize((needed as usize).div_ceil(8) + 1, 0);
        }
    };
    // SAFETY: the handle came from OpenSCManagerW above.
    unsafe { CloseServiceHandle(manager) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpapi_round_trips_and_detects_tampering() {
        let sealed = dpapi_protect(b"user\npa\"ss\\word").unwrap();
        assert!(!sealed.windows(4).any(|w| w == b"pa\"s"));
        assert_eq!(
            dpapi_unprotect(&sealed).unwrap().as_slice(),
            b"user\npa\"ss\\word"
        );

        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xFF;
        assert!(dpapi_unprotect(&tampered).is_err());
    }

    #[test]
    fn known_folders_resolve() {
        assert!(program_data().unwrap().is_absolute());
        assert!(program_files().unwrap().is_absolute());
    }

    #[test]
    fn a_directory_created_with_sddl_carries_exactly_that_dacl() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("secured");
        // Owner-only access keeps the test able to clean up after itself.
        let sddl = "D:P(A;OICI;FA;;;OW)";
        assert!(create_dir_with_sddl(&dir, sddl).unwrap());
        assert!(!create_dir_with_sddl(&dir, sddl).unwrap());
        assert_eq!(dacl_sddl(&dir).unwrap(), sddl);
        assert!(has_trusted_owner(&dir).unwrap());
        assert!(!is_reparse_point(&dir).unwrap());
    }

    #[test]
    fn set_dacl_replaces_inherited_entries() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("plain");
        std::fs::create_dir(&dir).unwrap();
        assert!(!dacl_sddl(&dir).unwrap().starts_with("D:P"));
        set_dacl(&dir, "D:P(A;OICI;FA;;;OW)").unwrap();
        // `AI`: set through the API, so it propagates to children; no `ID` entries remain.
        assert_eq!(dacl_sddl(&dir).unwrap(), "D:PAI(A;OICI;FA;;;OW)");
    }

    #[test]
    fn invalid_sddl_is_an_error() {
        assert!(SecurityDescriptor::from_sddl("not sddl").is_err());
    }

    #[test]
    fn services_are_found_by_prefix_without_regard_to_case() {
        // The event log service exists on every Windows installation.
        let found = services_with_prefix("EVENTLOG").unwrap();
        assert!(
            found.iter().any(|n| n.eq_ignore_ascii_case("EventLog")),
            "{found:?}"
        );
        assert!(
            services_with_prefix("WireGuardTunnel$th-zz-none")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn an_unknown_interface_has_no_luid() {
        assert!(interface_luid("th-not-an-interface").is_err());
    }
}
