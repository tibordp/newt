//! The Recycle Bin through the shell's `IFileOperation`, which is what
//! Explorer's Delete does. With `FOF_ALLOWUNDO` the shell still deletes
//! outright what the bin can't take (too large, or on a drive without one);
//! a progress sink sees that coming in `PreDeleteItem` and aborts instead.

use std::cell::Cell;
use std::path::{Component, Path, PathBuf, Prefix};

use windows::Win32::Foundation::{
    E_ABORT, E_ACCESSDENIED, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    IBindCtx,
};
use windows::Win32::UI::Shell::{
    FOF_ALLOWUNDO, FOF_NO_UI, FileOperation, IFileOperation, IFileOperationProgressSink,
    IFileOperationProgressSink_Impl, IShellItem, SHCreateItemFromParsingName,
    TSF_DELETE_RECYCLE_IF_POSSIBLE,
};
use windows::core::{ComObject, HRESULT, HSTRING, PCWSTR, Ref, implement};

use super::refused;
use newt_common::{Error, ErrorKind};

pub(super) fn trash(path: &Path) -> Result<(), Error> {
    let _apartment = Apartment::enter();
    let sink = ComObject::new(RecycleOnly {
        refused: Cell::new(false),
        failed: Cell::new(None),
    });
    // SAFETY: plain COM calls on interfaces this thread created, inside its
    // apartment.
    unsafe {
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(shell_error)?;
        operation
            .SetOperationFlags(FOF_NO_UI | FOF_ALLOWUNDO)
            .map_err(shell_error)?;
        let item: IShellItem = SHCreateItemFromParsingName(
            &HSTRING::from(shell_path(path).as_os_str()),
            None::<&IBindCtx>,
        )
        .map_err(shell_error)?;
        operation
            .DeleteItem(&item, &sink.to_interface::<IFileOperationProgressSink>())
            .map_err(shell_error)?;
        let performed = operation.PerformOperations();
        if sink.refused.get() {
            return Err(refused(
                "the Recycle Bin can't take it (it's too large, or the drive has none)",
            ));
        }
        if let Some(hr) = sink.failed.get() {
            return Err(shell_error(hr.into()));
        }
        performed.map_err(shell_error)?;
        if operation
            .GetAnyOperationsAborted()
            .map_err(shell_error)?
            .as_bool()
        {
            return Err(Error::custom("moving it to the Recycle Bin was aborted"));
        }
    }
    Ok(())
}

/// Lets a delete through only when the shell is going to recycle it.
#[implement(IFileOperationProgressSink)]
struct RecycleOnly {
    refused: Cell<bool>,
    failed: Cell<Option<HRESULT>>,
}

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for RecycleOnly_Impl {
    fn PreDeleteItem(&self, dwflags: u32, _item: Ref<'_, IShellItem>) -> windows::core::Result<()> {
        if dwflags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
            self.refused.set(true);
            return Err(E_ABORT.into());
        }
        Ok(())
    }

    fn PostDeleteItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        hrdelete: HRESULT,
        _newly_created: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        if hrdelete.is_err() {
            self.failed.set(Some(hrdelete));
        }
        Ok(())
    }

    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn FinishOperations(&self, _hrresult: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreRenameItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostRenameItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
        _hrrename: HRESULT,
        _newly_created: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreMoveItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostMoveItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
        _hrmove: HRESULT,
        _newly_created: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreCopyItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostCopyItem(
        &self,
        _dwflags: u32,
        _item: Ref<'_, IShellItem>,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
        _hrcopy: HRESULT,
        _newly_created: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreNewItem(
        &self,
        _dwflags: u32,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostNewItem(
        &self,
        _dwflags: u32,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
        _template_name: &PCWSTR,
        _attributes: u32,
        _hrnew: HRESULT,
        _new_item: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn UpdateProgress(&self, _work_total: u32, _work_so_far: u32) -> windows::core::Result<()> {
        Ok(())
    }

    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

/// COM for this thread for the length of the call: the shell's file
/// operations want a single-threaded apartment.
struct Apartment(bool);

impl Apartment {
    fn enter() -> Self {
        Self(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// `path` without the verbatim prefix the shell's parser rejects:
/// `\\?\C:\…` as `C:\…`, `\\?\UNC\host\share\…` as `\\host\share\…`.
fn shell_path(path: &Path) -> PathBuf {
    let mut components = path.components();
    let mut out = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:", drive as char)),
            Prefix::VerbatimUNC(host, share) => {
                let mut unc = std::ffi::OsString::from(r"\\");
                unc.push(host);
                unc.push(r"\");
                unc.push(share);
                PathBuf::from(unc)
            }
            _ => PathBuf::from(prefix.as_os_str()),
        },
        Some(component) => PathBuf::from(component.as_os_str()),
        None => PathBuf::new(),
    };
    out.extend(components);
    out
}

fn shell_error(e: windows::core::Error) -> Error {
    let code = e.code();
    let kind = if code == E_ACCESSDENIED {
        ErrorKind::PermissionDenied
    } else if code == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
        || code == HRESULT::from_win32(ERROR_PATH_NOT_FOUND.0)
    {
        ErrorKind::NotFound
    } else {
        ErrorKind::Other
    };
    Error {
        kind,
        message: e.message(),
    }
}
