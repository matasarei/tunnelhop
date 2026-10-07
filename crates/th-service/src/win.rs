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
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT, SE_KERNEL_OBJECT, SetNamedSecurityInfoW,
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
    CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OVERLAPPED,
    FILE_GENERIC_READ, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, GetFileVersionInfoSizeW,
    GetFileVersionInfoW, OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
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

// SAFETY: the descriptor is never modified after parsing, and only read by the system calls
// it is passed to, so sharing it between threads is sound.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    /// Attributes carrying this descriptor, for creating an object. They borrow `self`.
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: 0,
        }
    }

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
    let attributes = sd.attributes();
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
    dacl_to_sddl(LocalBox(sd))
}

/// Open the client end of a named pipe for overlapped I/O, asking for read, write-data and
/// write-attributes only: the pipe grants users no more (see `pipe::PIPE_SDDL`), so asking for
/// `GENERIC_WRITE` would be refused. The server may only identify the client, never act as it.
pub fn open_pipe_client(name: &str) -> io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;

    // SAFETY: NUL-terminated name; the returned handle is checked and then owned.
    let handle = unsafe {
        CreateFileW(
            wide(name).as_ptr(),
            FILE_GENERIC_READ | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES,
            0,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a valid handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// The Windows logon session of the process at the other end of a connected pipe. Each logged-in
/// user has their own, so it tells one user's connections from another's.
pub fn pipe_client_session(pipe: &impl std::os::windows::io::AsHandle) -> io::Result<u32> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeClientSessionId;

    let mut session = 0u32;
    // SAFETY: the borrow keeps the handle open; the output is a plain u32.
    if unsafe { GetNamedPipeClientSessionId(pipe.as_handle().as_raw_handle(), &mut session) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(session)
}

/// Whether a kernel object (the server side of a pipe, seen from its client) is owned by
/// SYSTEM or the Administrators group. A standard user cannot make either the owner of an
/// object they create.
pub fn owned_by_system_or_admins(object: &impl std::os::windows::io::AsHandle) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;

    let mut owner: PSID = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the borrow keeps the handle open; `owner` points into `sd`, freed by LocalBox.
    win32_result(unsafe {
        GetSecurityInfo(
            object.as_handle().as_raw_handle(),
            SE_KERNEL_OBJECT,
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
    Ok(unsafe {
        IsWellKnownSid(owner, WinLocalSystemSid) != 0
            || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
    })
}

/// The DACL of a kernel object (a pipe, say) by its handle, in SDDL.
pub fn handle_dacl_sddl(object: &impl std::os::windows::io::AsHandle) -> io::Result<String> {
    use std::os::windows::io::AsRawHandle;

    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the borrow keeps the handle open for the call; `sd` is freed by LocalBox.
    win32_result(unsafe {
        GetSecurityInfo(
            object.as_handle().as_raw_handle(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut sd,
        )
    })?;
    dacl_to_sddl(LocalBox(sd))
}

fn dacl_to_sddl(sd: LocalBox) -> io::Result<String> {
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
            // Not even one entry fitted: grow to what the SCM asked for, and at least double,
            // so a `needed` that is no larger than the buffer cannot repeat the same call forever.
            let wanted = (needed as usize).div_ceil(8) + 1;
            buffer.resize(wanted.max(buffer.len() * 2), 0);
        }
    };
    // SAFETY: the handle came from OpenSCManagerW above.
    unsafe { CloseServiceHandle(manager) };
    result
}

/// A Job Object that kills every process in it once its last handle closes — including when the
/// service itself dies, since the system closes a dead process's handles.
pub struct Job(std::os::windows::io::OwnedHandle);

impl Job {
    pub fn kill_on_close() -> io::Result<Job> {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        // SAFETY: an unnamed job with default security.
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh handle we own from here on.
        let job = Job(unsafe { OwnedHandle::from_raw_handle(raw) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the structure and its size match the information class.
        let ok = unsafe {
            SetInformationJobObject(
                raw,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub fn assign(&self, process: std::os::windows::io::BorrowedHandle<'_>) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        // SAFETY: both handles are open for the duration of the call.
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process.as_raw_handle()) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn terminate(&self) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        // SAFETY: the job handle is open.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Resume every thread of a process started with `CREATE_SUSPENDED` (one, its main thread).
pub fn resume_process(pid: u32) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    // SAFETY: a snapshot of every thread in the system; filtered by owner below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut resumed = 0;
    let mut result = Ok(());
    // SAFETY: the entry's size field is set, as both calls require.
    let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: a thread id from the snapshot; the handle is closed right after use.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                result = Err(io::Error::last_os_error());
                break;
            }
            // SAFETY: an open thread handle with THREAD_SUSPEND_RESUME.
            let previous = unsafe { ResumeThread(thread) };
            unsafe { CloseHandle(thread) };
            if previous == u32::MAX {
                result = Err(io::Error::last_os_error());
                break;
            }
            resumed += 1;
        }
        // SAFETY: as above.
        more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    // SAFETY: the snapshot handle came from CreateToolhelp32Snapshot.
    unsafe { CloseHandle(snapshot) };
    result?;
    if resumed == 0 {
        return Err(io::Error::other(format!("process {pid} has no threads")));
    }
    Ok(())
}

/// The processes listening on `port` over TCP on 127.0.0.1 or every IPv4 address.
pub fn tcp_listeners(port: u16) -> io::Result<Vec<u32>> {
    use std::net::Ipv4Addr;
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;

    // u32 storage keeps the buffer aligned for the table written into it.
    let mut buffer = vec![0u32; 1024];
    loop {
        let mut size = (buffer.len() * 4) as u32;
        // SAFETY: the buffer is as large as the size passed.
        let code = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut size,
                0,
                u32::from(AF_INET),
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if code == ERROR_INSUFFICIENT_BUFFER {
            buffer.resize((size as usize).div_ceil(4).max(buffer.len() * 2), 0);
            continue;
        }
        if code != NO_ERROR {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
        // SAFETY: a MIB_TCPTABLE_OWNER_PID: the row count, then that many rows.
        let rows = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().add(1) as *const MIB_TCPROW_OWNER_PID,
                buffer[0] as usize,
            )
        };
        return Ok(rows
            .iter()
            .filter(|row| u16::from_be(row.dwLocalPort as u16) == port)
            .filter(|row| {
                let addr = Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes());
                addr.is_loopback() || addr.is_unspecified()
            })
            .map(|row| row.dwOwningPid)
            .collect());
    }
}

/// An address in the form the IP Helper tables use.
fn ip_of(
    addr: &windows_sys::Win32::Networking::WinSock::SOCKADDR_INET,
) -> Option<std::net::IpAddr> {
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
    // SAFETY: the family field is shared by every variant and tells which one is set.
    unsafe {
        match addr.si_family {
            AF_INET => {
                Some(std::net::Ipv4Addr::from(addr.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()).into())
            }
            AF_INET6 => Some(std::net::Ipv6Addr::from(addr.Ipv6.sin6_addr.u.Byte).into()),
            _ => None,
        }
    }
}

/// The interface that holds `ip` as one of its unicast addresses, and whether the address is
/// usable yet. A new address is "tentative" while Windows checks it is not a duplicate (about
/// two seconds on an OpenVPN adapter), and until then nothing can be sent from it.
pub fn interface_with_address(ip: std::net::IpAddr) -> io::Result<Option<(u64, bool)>> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetUnicastIpAddressTable, MIB_UNICASTIPADDRESS_ROW,
        MIB_UNICASTIPADDRESS_TABLE,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_UNSPEC, IpDadStatePreferred};

    let mut table: *mut MIB_UNICASTIPADDRESS_TABLE = ptr::null_mut();
    // SAFETY: a valid out pointer; the table is freed below.
    win32_result(unsafe { GetUnicastIpAddressTable(AF_UNSPEC, &mut table) })?;
    // SAFETY: the call returned a table with NumEntries rows.
    let rows = unsafe {
        std::slice::from_raw_parts(
            (*table).Table.as_ptr() as *const MIB_UNICASTIPADDRESS_ROW,
            (*table).NumEntries as usize,
        )
    };
    let found = rows
        .iter()
        .find(|row| ip_of(&row.Address) == Some(ip))
        // SAFETY: every bit pattern of the union is a valid u64.
        .map(|row| {
            (
                unsafe { row.InterfaceLuid.Value },
                row.DadState == IpDadStatePreferred,
            )
        });
    // SAFETY: allocated by GetUnicastIpAddressTable.
    unsafe { FreeMibTable(table.cast()) };
    Ok(found)
}

/// Whether the interface exists and is operationally up (for a tunnel adapter: in use).
pub fn interface_is_up(luid: u64) -> io::Result<bool> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetIfEntry2, MIB_IF_ROW2};
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;

    if luid == 0 {
        return Ok(false);
    }
    let mut row = MIB_IF_ROW2::default();
    row.InterfaceLuid.Value = luid;
    // SAFETY: a row with its LUID set, as the call requires.
    match win32_result(unsafe { GetIfEntry2(&mut row) }) {
        Ok(()) => Ok(row.OperStatus == IfOperStatusUp),
        Err(e)
            if e.raw_os_error()
                == Some(windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND as i32) =>
        {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Delete the routes a killed OpenVPN leaves behind: every manually added (`NETMGMT`) route on
/// the tunnel's interface, and the host routes to its servers. Routes anyone else added on other
/// interfaces are never touched. Returns how many were deleted.
pub fn delete_tunnel_routes(interface: u64, servers: &[std::net::IpAddr]) -> io::Result<usize> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        DeleteIpForwardEntry2, FreeMibTable, GetIpForwardTable2, MIB_IPFORWARD_ROW2,
        MIB_IPFORWARD_TABLE2,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_UNSPEC, MIB_IPPROTO_NETMGMT};

    let mut table: *mut MIB_IPFORWARD_TABLE2 = ptr::null_mut();
    // SAFETY: a valid out pointer; the table is freed below.
    win32_result(unsafe { GetIpForwardTable2(AF_UNSPEC, &mut table) })?;
    // SAFETY: the call returned a table with NumEntries rows.
    let rows = unsafe {
        std::slice::from_raw_parts(
            (*table).Table.as_ptr() as *const MIB_IPFORWARD_ROW2,
            (*table).NumEntries as usize,
        )
    };
    let mut deleted = 0;
    let mut result = Ok(());
    for row in rows {
        if row.Protocol != MIB_IPPROTO_NETMGMT {
            continue;
        }
        // SAFETY: every bit pattern of the union is a valid u64.
        let on_tunnel = interface != 0 && unsafe { row.InterfaceLuid.Value } == interface;
        let prefix = &row.DestinationPrefix;
        let host = ip_of(&prefix.Prefix).filter(|ip| {
            let full = if ip.is_ipv4() { 32 } else { 128 };
            prefix.PrefixLength == full
        });
        let to_server = host.is_some_and(|ip| servers.contains(&ip));
        if on_tunnel || to_server {
            // SAFETY: a row exactly as the table returned it.
            match win32_result(unsafe { DeleteIpForwardEntry2(row) }) {
                Ok(()) => deleted += 1,
                Err(e) => result = Err(e),
            }
        }
    }
    // SAFETY: allocated by GetIpForwardTable2.
    unsafe { FreeMibTable(table.cast()) };
    result.map(|()| deleted)
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

    #[test]
    fn a_listener_is_found_with_its_owning_process() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_eq!(tcp_listeners(port).unwrap(), [std::process::id()]);
        drop(listener);
        assert!(tcp_listeners(port).unwrap().is_empty());
    }

    #[test]
    fn an_address_leads_to_its_interface() {
        let (loopback, usable) = interface_with_address("127.0.0.1".parse().unwrap())
            .unwrap()
            .expect("loopback holds 127.0.0.1");
        assert_ne!(loopback, 0);
        assert!(
            usable,
            "loopback's address is long past duplicate detection"
        );
        assert!(interface_is_up(loopback).unwrap());
        assert!(!interface_is_up(0).unwrap(), "no interface has LUID 0");
        // TEST-NET-1: never assigned to a real interface.
        assert_eq!(
            interface_with_address("192.0.2.123".parse().unwrap()).unwrap(),
            None
        );
    }

    #[test]
    fn the_route_table_is_read_and_nothing_matches_no_tunnel() {
        // No interface and no servers: nothing qualifies, so nothing on this machine changes.
        assert_eq!(delete_tunnel_routes(0, &[]).unwrap(), 0);
    }

    #[test]
    fn a_suspended_process_runs_once_resumed_and_dies_with_its_job() {
        use std::os::windows::io::AsHandle;
        use std::os::windows::process::CommandExt;
        use std::time::{Duration, Instant};
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let job = Job::kill_on_close().unwrap();
        job.assign(child.as_handle()).unwrap();
        resume_process(child.id()).unwrap();
        assert!(child.try_wait().unwrap().is_none(), "ping exited early");

        drop(job);
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "closing the job did not kill ping"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
