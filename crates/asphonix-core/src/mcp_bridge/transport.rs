//! Local-only listeners for the MCP bridge: a named pipe on Windows (explicit
//! current-user-only DACL, remote clients rejected, first-instance flag so the
//! name cannot be pre-created by someone else) and a Unix domain socket
//! (0600, in a 0700 directory) elsewhere. There is deliberately no TCP/HTTP
//! option.

use std::future::Future;
use std::io;
use std::pin::Pin;

use tokio::io::{AsyncRead, AsyncWrite};

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Stream for T {}
pub type BoxedStream = Box<dyn Stream>;

/// Accepts connections one at a time; `accept` must be cancel-safe (it is
/// raced against the shutdown signal).
pub trait Listener: Send + 'static {
    fn accept(&mut self) -> Pin<Box<dyn Future<Output = io::Result<BoxedStream>> + Send + '_>>;
}

#[cfg(windows)]
pub use win::bind;
#[cfg(unix)]
pub use unix::bind;

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::c_void;

    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// A security descriptor granting `GENERIC_ALL` to the current token's
    /// user SID only (no Everyone/Anonymous read like a pipe's default DACL;
    /// not "owner rights", which would be Administrators when elevated).
    struct PipeSecurity {
        sd: PSECURITY_DESCRIPTOR,
    }

    // The descriptor is immutable heap memory owned by this struct.
    unsafe impl Send for PipeSecurity {}

    impl Drop for PipeSecurity {
        fn drop(&mut self) {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.sd.0)));
            }
        }
    }

    fn current_user_sid_string() -> io::Result<String> {
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(io::Error::other)?;
            let result = (|| {
                let mut len = 0u32;
                // First call only reports the needed size (and "fails").
                let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
                if len == 0 {
                    return Err(io::Error::other("GetTokenInformation: no size"));
                }
                // u64 backing store: TOKEN_USER holds pointers and needs 8-byte alignment.
                let mut buf = vec![0u64; (len as usize).div_ceil(8)];
                GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as *mut c_void), len, &mut len)
                    .map_err(io::Error::other)?;
                let user = &*(buf.as_ptr() as *const TOKEN_USER);
                let mut s = PWSTR::null();
                ConvertSidToStringSidW(user.User.Sid, &mut s).map_err(io::Error::other)?;
                let out = s.to_string().map_err(io::Error::other);
                let _ = LocalFree(Some(HLOCAL(s.0 as *mut c_void)));
                out
            })();
            let _ = CloseHandle(token);
            result
        }
    }

    impl PipeSecurity {
        fn current_user_only() -> io::Result<Self> {
            let sddl = format!("D:P(A;;GA;;;{})", current_user_sid_string()?);
            let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
            let mut sd = PSECURITY_DESCRIPTOR::default();
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(wide.as_ptr()), SDDL_REVISION_1, &mut sd, None)
                    .map_err(io::Error::other)?;
            }
            Ok(Self { sd })
        }

        fn create(&self, name: &str, first: bool) -> io::Result<NamedPipeServer> {
            let mut attrs = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.sd.0,
                bInheritHandle: false.into(),
            };
            let mut opts = ServerOptions::new();
            opts.first_pipe_instance(first).reject_remote_clients(true).max_instances(16);
            // SAFETY: `attrs` is a valid SECURITY_ATTRIBUTES for the duration of
            // the call (the kernel copies the descriptor into the pipe object).
            unsafe { opts.create_with_security_attributes_raw(name, &mut attrs as *mut _ as *mut c_void) }
        }
    }

    pub struct PipeListener {
        name: String,
        security: PipeSecurity,
        next: NamedPipeServer,
    }

    /// Creates the pipe `name` (must be inside a tokio runtime).
    pub fn bind(name: &str) -> io::Result<PipeListener> {
        let security = PipeSecurity::current_user_only()?;
        let next = security.create(name, true)?;
        Ok(PipeListener { name: name.to_string(), security, next })
    }

    impl Listener for PipeListener {
        fn accept(&mut self) -> Pin<Box<dyn Future<Output = io::Result<BoxedStream>> + Send + '_>> {
            Box::pin(async move {
                if let Err(e) = self.next.connect().await {
                    // A client that connected and vanished (or any connect error) leaves
                    // this instance unusable: retrying it would fail forever. Replace it.
                    if let Ok(fresh) = self.security.create(&self.name, false) {
                        self.next = fresh;
                    }
                    return Err(e);
                }
                let fresh = self.security.create(&self.name, false)?;
                let connected = std::mem::replace(&mut self.next, fresh);
                Ok(Box::new(connected) as BoxedStream)
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
        };
        use windows::Win32::Security::DACL_SECURITY_INFORMATION;

        /// Reads the DACL the kernel actually stored on the pipe (through a
        /// client handle) and checks it is exactly "current user, full access,
        /// protected": no Everyone/Anonymous/Administrators/AppContainer ACE.
        #[tokio::test]
        async fn pipe_dacl_is_current_user_only() {
            let name = format!(r"\\.\pipe\asphonix-mcp-test-dacl-{}", std::process::id());
            let _listener = bind(&name).unwrap();
            let client = std::fs::OpenOptions::new().read(true).write(true).open(&name).unwrap();
            let sddl = unsafe {
                let mut sd = PSECURITY_DESCRIPTOR::default();
                let err = GetSecurityInfo(
                    HANDLE(client.as_raw_handle()),
                    SE_KERNEL_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    None,
                    None,
                    Some(&mut sd),
                );
                assert_eq!(err.0, 0, "GetSecurityInfo failed");
                let mut s = PWSTR::null();
                ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, SDDL_REVISION_1, DACL_SECURITY_INFORMATION, &mut s, None)
                    .unwrap();
                let out = s.to_string().unwrap();
                let _ = LocalFree(Some(HLOCAL(s.0 as *mut c_void)));
                let _ = LocalFree(Some(HLOCAL(sd.0)));
                out
            };
            // GENERIC_ALL is stored mapped to the file-object rights (FA).
            // Round-trip the expected DACL through the same SDDL conversion:
            // Windows prints well-known accounts as aliases (the built-in
            // Administrator, which CI runners use, comes out as "LA"), so a
            // raw SID string would only match for ordinary accounts.
            let expected = canonical_dacl_sddl(&format!("D:P(A;;FA;;;{})", current_user_sid_string().unwrap()));
            assert_eq!(sddl, expected);
        }

        fn canonical_dacl_sddl(sddl: &str) -> String {
            let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                let mut sd = PSECURITY_DESCRIPTOR::default();
                ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(wide.as_ptr()), SDDL_REVISION_1, &mut sd, None)
                    .unwrap();
                let mut s = PWSTR::null();
                ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, SDDL_REVISION_1, DACL_SECURITY_INFORMATION, &mut s, None)
                    .unwrap();
                let out = s.to_string().unwrap();
                let _ = LocalFree(Some(HLOCAL(s.0 as *mut c_void)));
                let _ = LocalFree(Some(HLOCAL(sd.0)));
                out
            }
        }
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tokio::net::UnixListener;

    pub struct UnixSocketListener {
        inner: UnixListener,
        path: PathBuf,
        /// (device, inode) of the socket file this listener created.
        id: (u64, u64),
    }

    fn file_id(path: &Path) -> io::Result<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::symlink_metadata(path)?;
        Ok((m.dev(), m.ino()))
    }

    /// Binds `path` (its parent directory becomes 0700, the socket 0600). The
    /// caller picks a fresh random file name per enable.
    pub fn bind(path: &Path) -> io::Result<UnixSocketListener> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let _ = std::fs::remove_file(path);
        let inner = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let id = file_id(path)?;
        Ok(UnixSocketListener { inner, path: path.to_path_buf(), id })
    }

    impl Drop for UnixSocketListener {
        /// Removes the socket file only while it is still the one this
        /// listener created (never another listener's socket at that path).
        fn drop(&mut self) {
            if file_id(&self.path).ok() == Some(self.id) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    impl Listener for UnixSocketListener {
        fn accept(&mut self) -> Pin<Box<dyn Future<Output = io::Result<BoxedStream>> + Send + '_>> {
            Box::pin(async move {
                let (s, _) = self.inner.accept().await?;
                Ok(Box::new(s) as BoxedStream)
            })
        }
    }
}
