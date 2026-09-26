use std::ffi::OsStr;
use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    ACE_HEADER, ACL, GetAce, GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation,
    LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_ATTRIBUTES, SYSTEM_MANDATORY_LABEL_ACE, TOKEN_INFORMATION_CLASS,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER, TokenIntegrityLevel, TokenUser,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows_sys::Win32::System::SystemServices::SYSTEM_MANDATORY_LABEL_ACE_TYPE;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// `SECURITY_MANDATORY_MEDIUM_RID`: a normal, non-elevated user process.
const MEDIUM: u32 = 0x2000;
const HIGH: u32 = 0x3000;
const SYSTEM: u32 = 0x4000;

/// A user's security identifier, in its string form (`S-1-5-21-…`).
///
/// Compared as a whole string: two SIDs are the same user exactly when
/// their canonical forms are equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserSid(String);

impl UserSid {
    /// The `S-1-…` form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for UserSid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The user this process runs as.
///
/// # Errors
/// The token cannot be queried (it always can for one's own process).
pub fn current_user() -> io::Result<UserSid> {
    let token = own_token()?;
    let buf = token_info(&token, TokenUser)?;
    #[allow(unsafe_code)]
    // SAFETY: `buf` holds a TOKEN_USER (the query succeeded) whose `Sid`
    // points inside `buf`, alive until `sid_string` returns.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    sid_string(sid)
}

/// Creates one instance of the pipe `name` that only the current user can
/// open, refusing remote clients.
///
/// Its security descriptor is explicit, so a client can CHECK it
/// ([`server_is_ours`]): owner = this user; DACL = this user only; mandatory
/// label = this process's own integrity, so an elevated daemon is not
/// reachable from medium integrity and a low-integrity process cannot even
/// read.
///
/// `first` asks for the FIRST instance: it fails if the name already exists,
/// whoever owns it. That is how a daemon learns another one is running, and
/// also how a pipe squatted by someone else is noticed instead of shared.
///
/// # Errors
/// The name exists and `first` was asked (`PermissionDenied`), or pipe I/O.
#[allow(unsafe_code)]
pub fn create_server(name: &OsStr, first: bool) -> io::Result<NamedPipeServer> {
    let me = current_user()?;
    let label = match own_integrity()? {
        rid if rid < MEDIUM => "LW",
        rid if rid < HIGH => "ME",
        rid if rid < SYSTEM => "HI",
        _ => "SI",
    };
    // `P`: nothing inherited. `NWNR`: below the label, neither write nor read.
    create_with(
        name,
        first,
        &format!("O:{me}D:P(A;;GA;;;{me})S:(ML;;NWNR;;;{label})"),
    )
}

#[allow(unsafe_code)]
fn create_with(name: &OsStr, first: bool, sddl: &str) -> io::Result<NamedPipeServer> {
    let sddl: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `sddl` is NUL-terminated and outlives the call; `descriptor`
    // receives a `LocalAlloc`ed pointer that `Local` frees below.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = Local(descriptor);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_u32::<SECURITY_ATTRIBUTES>(),
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    // SAFETY: `attributes` is a valid SECURITY_ATTRIBUTES whose descriptor
    // (`descriptor`) stays alive until after the call returns; the pipe
    // copies the security it needs at creation.
    unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) }
}

/// The user of the process that connected to `pipe`.
///
/// A second check behind the DACL, which already admits only this user:
/// a PID resolved after the fact can only fail closed here.
///
/// # Errors
/// The client already exited, or its token cannot be queried.
#[allow(unsafe_code)]
pub fn client_user(pipe: &NamedPipeServer) -> io::Result<UserSid> {
    let mut pid = 0u32;
    // SAFETY: the handle is alive for the call (`pipe` is borrowed) and
    // `pid` is a valid out-pointer.
    let ok = unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &raw mut pid) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: plain call; a null result is checked before use.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a fresh process handle nobody else owns.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let token = process_token(process.as_raw_handle())?;
    let buf = token_info(&token, TokenUser)?;
    // SAFETY: as in `current_user`.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    sid_string(sid)
}

/// Is the pipe `pipe` connected to one that THIS user's daemon created?
///
/// Read from the pipe object, not from a PID: its owner must be this user,
/// which another user cannot set without restore privilege, and its
/// integrity label must be at least medium, which a sandboxed process of
/// this same user cannot give the pipe it creates.
///
/// # Errors
/// The pipe's security cannot be read.
#[allow(unsafe_code)]
pub fn server_is_ours(pipe: &NamedPipeClient) -> io::Result<bool> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut sacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the handle is alive for the call and has READ_CONTROL; every
    // out-pointer is valid. `owner` and `sacl` point into `descriptor`,
    // which `Local` frees after the last read.
    let status = unsafe {
        GetSecurityInfo(
            pipe.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            &raw mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut sacl,
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(i32::MAX),
        ));
    }
    let _descriptor = Local(descriptor);
    if owner.is_null() || sid_string(owner)? != current_user()? {
        return Ok(false);
    }
    // No label ACE at all means medium: that is Windows' default.
    let label = if sacl.is_null() {
        MEDIUM
    } else {
        label_of(sacl).unwrap_or(MEDIUM)
    };
    Ok(label >= MEDIUM)
}

/// The integrity RID in a SACL's mandatory-label ACE, if it has one.
#[allow(unsafe_code)]
fn label_of(sacl: *mut ACL) -> Option<u32> {
    // SAFETY: `sacl` is a valid ACL inside a live security descriptor.
    let count = unsafe { (*sacl).AceCount };
    for i in 0..u32::from(count) {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `i` is below the ACE count; `ace` is a valid out-pointer.
        if unsafe { GetAce(sacl, i, &raw mut ace) } == 0 {
            continue;
        }
        // SAFETY: every ACE starts with an ACE_HEADER.
        let header = unsafe { *ace.cast::<ACE_HEADER>() };
        if u32::from(header.AceType) == SYSTEM_MANDATORY_LABEL_ACE_TYPE {
            // SAFETY: a label ACE carries its SID from `SidStart` on.
            let sid = unsafe {
                ace.cast::<u8>()
                    .add(std::mem::offset_of!(SYSTEM_MANDATORY_LABEL_ACE, SidStart))
            };
            return Some(last_sub_authority(sid.cast()));
        }
    }
    None
}

/// This process's integrity RID.
fn own_integrity() -> io::Result<u32> {
    let token = own_token()?;
    let buf = token_info(&token, TokenIntegrityLevel)?;
    #[allow(unsafe_code)]
    // SAFETY: `buf` holds a TOKEN_MANDATORY_LABEL whose SID points inside
    // `buf`, alive for this call.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid };
    Ok(last_sub_authority(sid))
}

/// An integrity SID's RID is its last sub-authority.
#[allow(unsafe_code)]
fn last_sub_authority(sid: PSID) -> u32 {
    // SAFETY: `sid` is a valid SID; both calls only index inside it, and
    // the count is at least one for an integrity SID.
    unsafe {
        let count = u32::from(*GetSidSubAuthorityCount(sid));
        *GetSidSubAuthority(sid, count.saturating_sub(1))
    }
}

#[allow(unsafe_code)]
fn own_token() -> io::Result<OwnedHandle> {
    // SAFETY: a pseudo-handle that needs no closing and is always valid.
    process_token(unsafe { GetCurrentProcess() })
}

#[allow(unsafe_code)]
fn process_token(process: HANDLE) -> io::Result<OwnedHandle> {
    let mut raw: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is alive for the call; `raw` is a valid out-pointer.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a fresh token handle nobody else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// A token information class, in `u64` words: its structures hold pointers
/// and must be aligned for them.
#[allow(unsafe_code)]
fn token_info(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
    let mut needed = 0u32;
    // SAFETY: a size query: no buffer, and `needed` is a valid out-pointer.
    // It fails by design with ERROR_INSUFFICIENT_BUFFER.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            std::ptr::null_mut(),
            0,
            &raw mut needed,
        )
    };
    let mut buf = vec![0u64; usize::try_from(needed).unwrap_or(0).div_ceil(8)];
    let size = u32::try_from(buf.len() * 8).unwrap_or(u32::MAX);
    // SAFETY: `buf` is an owned, aligned buffer of `size` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            buf.as_mut_ptr().cast(),
            size,
            &raw mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

#[allow(unsafe_code)]
fn sid_string(sid: PSID) -> io::Result<UserSid> {
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sid` is a valid SID; `text` receives a `LocalAlloc`ed,
    // NUL-terminated string.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let text = Local(text.cast());
    let wide: *const u16 = text.0.cast();
    let mut len = 0;
    // SAFETY: NUL-terminated per the call's contract; read up to the NUL.
    while unsafe { *wide.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` units were just read one by one, all inside the string.
    let units = unsafe { std::slice::from_raw_parts(wide, len) };
    // A SID string is ASCII: nothing is lost.
    Ok(UserSid(String::from_utf16_lossy(units)))
}

fn size_u32<T>() -> u32 {
    u32::try_from(size_of::<T>()).unwrap_or(u32::MAX)
}

/// A `LocalAlloc`ed pointer, freed on drop.
struct Local(*mut core::ffi::c_void);

impl Drop for Local {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: every `Local` holds a pointer the system allocated with
        // `LocalAlloc` for us, freed exactly once, here.
        unsafe { LocalFree(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::windows::named_pipe::ClientOptions;

    fn unique_name(tag: &str) -> String {
        format!(r"\\.\pipe\norte-winpipe-test-{tag}-{}", std::process::id())
    }

    #[test]
    fn the_current_user_is_a_sid() {
        let me = current_user().expect("own token");
        assert!(me.as_str().starts_with("S-1-"), "{me}");
        assert!(
            own_integrity().unwrap() >= MEDIUM,
            "tests run at medium or above"
        );
    }

    /// Both ends check the other: the daemon the client's user, the client
    /// the pipe's owner and label.
    #[tokio::test]
    async fn each_end_accepts_the_other() {
        let name = unique_name("peer");
        let mut server = create_server(OsStr::new(&name), true).expect("create");
        let mut client = ClientOptions::new().open(&name).expect("open");
        server.connect().await.expect("connect");
        assert_eq!(client_user(&server).unwrap(), current_user().unwrap());
        assert!(server_is_ours(&client).unwrap());
        client.write_all(b"hi").await.unwrap();
        let mut buf = [0u8; 2];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hi", "and the DACL let us in");
    }

    /// A pipe labelled LOW — what a sandboxed process of this same user
    /// creates — is not the daemon's, even with our SID as its owner.
    #[tokio::test]
    async fn a_low_integrity_pipe_is_not_the_daemons() {
        let name = unique_name("low");
        let me = current_user().unwrap();
        let _server = create_with(
            OsStr::new(&name),
            true,
            &format!("O:{me}D:P(A;;GA;;;{me})S:(ML;;NW;;;LW)"),
        )
        .expect("create");
        let client = ClientOptions::new().open(&name).expect("open");
        assert!(!server_is_ours(&client).unwrap());
    }

    /// A second FIRST instance of a live name fails: two daemons cannot
    /// share one pipe, and neither can a squatter and a daemon.
    #[tokio::test]
    async fn a_name_in_use_refuses_a_first_instance() {
        let name = unique_name("first");
        let _live = create_server(OsStr::new(&name), true).expect("create");
        let err = create_server(OsStr::new(&name), true).expect_err("in use");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        assert!(
            create_server(OsStr::new(&name), false).is_ok(),
            "a later instance of our own pipe is fine"
        );
    }
}
