//! Handle-based Windows file operations.
//!
//! Paths are resolved once, at an approved root; everything below it is opened
//! relative to an already-open directory handle with `NtCreateFile`
//! (`RootDirectory` + one name component + `FILE_OPEN_REPARSE_POINT`), so a
//! descendant junction or symlink is opened as itself and refused instead of
//! followed. Share modes exclude concurrent writers/deleters for as long as a
//! handle is held, and renames/deletes act on the validated handle itself,
//! never on a pathname that could have been swapped.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF,
    FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
};
use windows_sys::Win32::Foundation::{OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ID_INFO,
    FILE_INFO_BY_HANDLE_CLASS, FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES,
    FILE_RENAME_INFO, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FILE_WRITE_ATTRIBUTES,
    FileDispositionInfo, FileDispositionInfoEx, FileIdInfo, FileRenameInfo,
    GetFileInformationByHandle, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
    SYNCHRONIZE, SetFileInformationByHandle, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

pub(crate) const ERROR_SHARING_VIOLATION: i32 = 32;

const DIR_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
/// Directories are shared for read/write but not delete: while a handle is
/// held, nobody can rename, move, or delete that directory.
const DIR_SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
/// Owned files: other readers may coexist, but no writer, rename, or delete can
/// start while the handle is held, and opening fails if one is already active.
const FILE_SHARE: u32 = FILE_SHARE_READ;
const OWNED_FILE_ACCESS: u32 = FILE_GENERIC_READ | DELETE | FILE_WRITE_ATTRIBUTES;

/// Volume serial + 128-bit file ID: identifies one file object on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    volume: u64,
    id: [u8; 16],
}

impl FileIdentity {
    pub(crate) fn to_bytes(self) -> [u8; 24] {
        let mut b = [0u8; 24];
        b[..8].copy_from_slice(&self.volume.to_le_bytes());
        b[8..].copy_from_slice(&self.id);
        b
    }

    pub(crate) fn from_bytes(b: &[u8]) -> Option<Self> {
        let b: &[u8; 24] = b.try_into().ok()?;
        let (volume, id) = b.split_at(8);
        Some(FileIdentity {
            volume: u64::from_le_bytes(volume.try_into().ok()?),
            id: id.try_into().ok()?,
        })
    }
}

pub(crate) fn identity(file: &File) -> io::Result<FileIdentity> {
    let h = file.as_raw_handle();
    let mut info = FILE_ID_INFO::default();
    // SAFETY: `info` is a writable FILE_ID_INFO of the size passed; `h` is open.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            h,
            FileIdInfo,
            (&raw mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok != 0 {
        return Ok(FileIdentity {
            volume: info.VolumeSerialNumber,
            id: info.FileId.Identifier,
        });
    }
    // File systems without 128-bit IDs (for example FAT) report the 64-bit index.
    let mut legacy = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `legacy` is a writable BY_HANDLE_FILE_INFORMATION; `h` is open.
    if unsafe { GetFileInformationByHandle(h, &mut legacy) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let index = (u64::from(legacy.nFileIndexHigh) << 32) | u64::from(legacy.nFileIndexLow);
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&index.to_le_bytes());
    Ok(FileIdentity {
        volume: u64::from(legacy.dwVolumeSerialNumber),
        id,
    })
}

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

fn is_reparse(file: &File) -> io::Result<bool> {
    Ok(file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

fn regular(file: File) -> io::Result<File> {
    let meta = file.metadata()?;
    if !meta.is_file() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid("not a regular file"));
    }
    Ok(file)
}

/// Opens the approved root directory by path. Reparse points along this path
/// are followed on purpose (a redirected Pictures folder is a legitimate
/// root); the returned handle is the binding every descendant is opened from.
pub(crate) fn open_root(path: &Path) -> io::Result<File> {
    let dir = OpenOptions::new()
        .access_mode(DIR_ACCESS)
        .share_mode(DIR_SHARE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    if !dir.metadata()?.is_dir() {
        return Err(invalid("not a directory"));
    }
    Ok(dir)
}

/// Opens a regular file by path for reading while denying writers, renames,
/// and deletes (fails with ERROR_SHARING_VIOLATION if one is already open).
/// A final-component symlink/junction is opened as itself and refused.
pub(crate) fn open_read_exclusive(path: &Path) -> io::Result<File> {
    regular(
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?,
    )
}

/// Like `open_read_exclusive`, with DELETE access for `delete_by_handle`.
pub(crate) fn open_owned_by_path(path: &Path) -> io::Result<File> {
    regular(
        OpenOptions::new()
            .access_mode(OWNED_FILE_ACCESS)
            .share_mode(FILE_SHARE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?,
    )
}

/// Opens (or creates) the directory `name` directly inside `parent`. A junction
/// or other reparse point is refused rather than followed.
pub(crate) fn open_child_dir(parent: &File, name: &str, create: bool) -> io::Result<File> {
    let disposition = if create { FILE_OPEN_IF } else { FILE_OPEN };
    let dir = nt_open(
        parent,
        name,
        DIR_ACCESS,
        DIR_SHARE,
        disposition,
        FILE_DIRECTORY_FILE,
    )?;
    if is_reparse(&dir)? {
        return Err(invalid("directory is a reparse point"));
    }
    Ok(dir)
}

/// Creates a new file `name` inside `parent`; fails if anything exists there.
pub(crate) fn create_child_file(parent: &File, name: &str) -> io::Result<File> {
    nt_open(
        parent,
        name,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
        FILE_SHARE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE,
    )
}

/// Opens the existing regular file `name` inside `parent` for verification
/// and deletion by handle. Reparse points are refused.
pub(crate) fn open_child_file(parent: &File, name: &str) -> io::Result<File> {
    regular(nt_open(
        parent,
        name,
        OWNED_FILE_ACCESS,
        FILE_SHARE,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
    )?)
}

/// One name component: no separators, streams, wildcards, or dot segments.
fn check_component(name: &str) -> io::Result<()> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.encode_utf16().count() > 255
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        });
    if bad {
        return Err(invalid("not a single file name component"));
    }
    Ok(())
}

fn nt_open(
    parent: &File,
    name: &str,
    access: u32,
    share: u32,
    disposition: u32,
    options: u32,
) -> io::Result<File> {
    check_component(name)?;
    let wide: Vec<u16> = name.encode_utf16().collect();
    let bytes = u16::try_from(wide.len() * 2).map_err(|_| invalid("name too long"))?;
    let object_name = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: wide.as_ptr().cast_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &object_name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut handle = ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer refers to a live local (`wide` outlives the call);
    // `parent` is an open directory handle; no EA buffer is passed. Synchronous
    // I/O (with SYNCHRONIZE access) keeps the handle usable by std::fs::File.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access | SYNCHRONIZE,
            &attributes,
            &mut status_block,
            ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            share,
            disposition,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: pure status-code translation.
        let code = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    // SAFETY: NtCreateFile succeeded, so `handle` is a new handle we now own.
    Ok(File::from(unsafe { OwnedHandle::from_raw_handle(handle) }))
}

/// Renames the open `file` to `name` inside `dir` without replacing an
/// existing entry (`AlreadyExists`). The object renamed is the one the handle
/// refers to; no pathname is reopened.
pub(crate) fn rename_in(file: &File, dir: &File, name: &str) -> io::Result<()> {
    check_component(name)?;
    let wide: Vec<u16> = name.encode_utf16().collect();
    let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let size = offset + (wide.len() + 1) * 2;
    let mut buf = vec![0u64; size.div_ceil(8)];
    let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: `buf` is zeroed, 8-byte aligned, and large enough for the header
    // plus `wide` and a NUL; both handles are open for the call's duration.
    let ok = unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = dir.as_raw_handle();
        (*info).FileNameLength = (wide.len() * 2) as u32;
        ptr::copy_nonoverlapping(
            wide.as_ptr(),
            (&raw mut (*info).FileName).cast::<u16>(),
            wide.len(),
        );
        SetFileInformationByHandle(file.as_raw_handle(), FileRenameInfo, info.cast(), size as u32)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_info<T>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS, value: &T) -> io::Result<()> {
    // SAFETY: `value` is a valid, initialized struct of the size passed for `class`.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            class,
            ptr::from_ref(value).cast::<c_void>(),
            size_of::<T>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Deletes the file this handle refers to (it disappears when the handle
/// closes). The read-only attribute is ignored by the delete itself rather
/// than cleared by path.
pub(crate) fn delete_by_handle(file: &File) -> io::Result<()> {
    let ex = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    if set_info(file, FileDispositionInfoEx, &ex).is_ok() {
        return Ok(());
    }
    // File systems without the extended class (for example FAT): clear
    // read-only on this same handle, delete, and restore it if that fails.
    let meta = file.metadata()?;
    let readonly = meta.file_attributes() & FILE_ATTRIBUTE_READONLY != 0;
    if readonly {
        let mut perms = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        file.set_permissions(perms)?;
    }
    let result = set_info(file, FileDispositionInfo, &FILE_DISPOSITION_INFO { DeleteFile: true });
    if result.is_err() && readonly {
        let mut perms = file.metadata()?.permissions();
        perms.set_readonly(true);
        let _ = file.set_permissions(perms);
    }
    result
}

/// Normalized `\\?\`-prefixed DOS path of an open handle (junctions, mapped
/// drives, and symlinks resolved).
pub(crate) fn final_path(file: &File) -> io::Result<String> {
    let mut buf = vec![0u16; 512];
    loop {
        let cap = u32::try_from(buf.len()).map_err(|_| invalid("path too long"))?;
        // SAFETY: `buf` has `cap` writable u16s; the handle is open.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buf.as_mut_ptr(),
                cap,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        match n {
            0 => return Err(io::Error::last_os_error()),
            n if n < buf.len() => {
                return String::from_utf16(&buf[..n]).map_err(|_| invalid("path is not Unicode"));
            }
            n if n <= 32 * 1024 => buf.resize(n + 1, 0),
            _ => return Err(invalid("path too long")),
        }
    }
}
