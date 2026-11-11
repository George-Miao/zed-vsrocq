use snafu::{ResultExt, Snafu};

#[derive(Debug, Snafu)]
pub(crate) enum Error {
    #[snafu(display("cannot generate authentication token: {source}"))]
    GenerateToken { source: getrandom::Error },

    #[cfg(windows)]
    #[snafu(display("cannot create named-pipe security descriptor: {source}"))]
    CreateDescriptor { source: windows::core::Error },

    #[cfg(windows)]
    #[snafu(display("cannot create secure named pipe: {source}"))]
    CreatePipe { source: std::io::Error },

    #[cfg(windows)]
    #[snafu(display("cannot attach secure named pipe to the runtime: {source}"))]
    AttachPipe { source: std::io::Error },
}

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn token() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).context(GenerateTokenSnafu)?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = String::with_capacity(64);
    for byte in bytes {
        token.push(HEX[(byte >> 4) as usize] as char);
        token.push(HEX[(byte & 15) as usize] as char);
    }
    Ok(token)
}

#[cfg(windows)]
pub(crate) fn pipe(name: &str, inbound: bool) -> Result<compio::fs::named_pipe::NamedPipeServer> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};

    use compio::{fs::named_pipe::NamedPipeServer, runtime::Runtime};
    use windows::{
        Win32::{
            Foundation::{HLOCAL, LocalFree},
            Security::{
                Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
                PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
            },
            Storage::FileSystem::{
                FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
            },
            System::Pipes::{CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE},
        },
        core::{PCWSTR, w},
    };

    let mut descriptor = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
    // SAFETY: the SDDL is NUL-terminated, and descriptor is a valid output
    // location. The protected DACL grants access only to the owner and SYSTEM.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("D:P(A;;GA;;;SY)(A;;GA;;;OW)"),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .context(CreateDescriptorSnafu)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let access = if inbound {
        PIPE_ACCESS_INBOUND
    } else {
        PIPE_ACCESS_OUTBOUND
    };
    // SAFETY: name is NUL-terminated; attributes and its descriptor remain
    // valid through creation. The parent endpoint is genuinely overlapped.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            access | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            65536,
            65536,
            0,
            Some(&attributes),
        )
    };
    let error = handle.is_invalid().then(std::io::Error::last_os_error);
    // SAFETY: the conversion API allocated this descriptor with LocalAlloc.
    unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    if let Some(error) = error {
        return Err(error).context(CreatePipeSnafu);
    }
    // SAFETY: CreateNamedPipeW returned a valid, uniquely owned handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    Runtime::with_current(|runtime| runtime.attach(handle.as_raw_handle() as _)).context(AttachPipeSnafu)?;
    // SAFETY: this is an overlapped named-pipe server endpoint attached to
    // the current runtime. Ownership transfers exactly once into Compio.
    Ok(unsafe { NamedPipeServer::from_raw_handle(handle.into_raw_handle()) })
}
