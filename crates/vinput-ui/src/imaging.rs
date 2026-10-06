//! Bitmaps in memory: the surface a picture is drawn on, the PNG file it is
//! saved as, and the icon of the program as something `Direct2D` can draw.
#![allow(unsafe_code)]

use std::path::Path;

use windows::Win32::Foundation::{GENERIC_WRITE, HINSTANCE};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA,
    GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICBitmapSource, IWICImagingFactory,
    WICBitmapCacheOnLoad, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache,
    WICBitmapPaletteTypeCustom,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW,
};
use windows::core::{HSTRING, Interface, PCWSTR};

use crate::error::{Error, Result};
use crate::gfx::failed;

/// The resource the program's icon is stored under.
const APP_ICON: u16 = 1;

/// Keeps COM usable on this thread for as long as it lives.
pub struct Com {
    owned: bool,
}

impl Com {
    /// Makes COM usable on this thread. If the thread set it up already, in
    /// whatever way, that is used as it is.
    #[must_use]
    pub fn enter() -> Self {
        // SAFETY: a plain call; `Drop` undoes it only if it took effect.
        let owned = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        Self { owned }
    }

    /// Whether COM objects made on this thread are only ever called on it
    /// (a single-threaded apartment). `false` if the thread had set COM up
    /// the other way before, for calls from any thread.
    #[must_use]
    pub fn apartment_threaded(&self) -> bool {
        self.owned
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: balances the successful `CoInitializeEx` in `enter`.
            unsafe { CoUninitialize() };
        }
    }
}

/// An icon handle that is given back to Windows when it goes out of scope.
struct Icon(HICON);

impl Drop for Icon {
    fn drop(&mut self) {
        // SAFETY: the handle came from `LoadImageW` and is not used again.
        let _ = unsafe { DestroyIcon(self.0) };
    }
}

/// The Windows Imaging Component.
pub struct Imaging {
    factory: IWICImagingFactory,
    _com: Com,
}

impl Imaging {
    /// Starts the Windows Imaging Component.
    ///
    /// # Errors
    ///
    /// Returns [`Error::System`] if it cannot be started.
    pub fn new() -> Result<Self> {
        let com = Com::enter();
        // SAFETY: COM is usable on this thread; the result is checked.
        let factory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .map_err(failed("starting the imaging component"))?;
        Ok(Self { factory, _com: com })
    }

    /// An empty bitmap of `width` by `height` pixels to draw on.
    ///
    /// # Errors
    ///
    /// Returns [`Error::System`] if there is no memory for it.
    pub fn bitmap(&self, width: u32, height: u32) -> Result<IWICBitmap> {
        // SAFETY: the format is a valid GUID for the call.
        unsafe {
            self.factory.CreateBitmap(
                width.max(1),
                height.max(1),
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapCacheOnLoad,
            )
        }
        .map_err(failed("making room for the picture"))
    }

    /// Writes `bitmap` to `path` as a PNG file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Picture`] if the file cannot be written.
    pub fn save_png(&self, bitmap: &IWICBitmap, path: &Path) -> Result<()> {
        let name = HSTRING::from(path.as_os_str());
        let write = || -> windows::core::Result<()> {
            // SAFETY: every pointer passed is valid for its call, the file
            // name is NUL-terminated, and each object is checked before it
            // is used.
            unsafe {
                let stream = self.factory.CreateStream()?;
                stream.InitializeFromFilename(&name, GENERIC_WRITE.0)?;
                let encoder = self
                    .factory
                    .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
                encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
                let mut frame = None;
                encoder.CreateNewFrame(&raw mut frame, std::ptr::null_mut())?;
                let frame = frame.ok_or_else(windows::core::Error::empty)?;
                frame.Initialize(None)?;
                let (mut width, mut height) = (0, 0);
                bitmap.GetSize(&raw mut width, &raw mut height)?;
                frame.SetSize(width, height)?;
                let mut format = GUID_WICPixelFormat32bppBGRA;
                frame.SetPixelFormat(&raw mut format)?;
                frame.WriteSource(bitmap, std::ptr::null())?;
                frame.Commit()?;
                encoder.Commit()
            }
        };
        write().map_err(|error| Error::Picture {
            path: path.to_owned(),
            code: error.code().0,
        })
    }

    /// The icon of the program at `pixels` square, in the format `Direct2D`
    /// draws. `None` if the program has no icon, as a test binary has not.
    #[must_use]
    pub fn app_icon(&self, pixels: u32) -> Option<IWICBitmapSource> {
        let side = i32::try_from(pixels).ok()?;
        // SAFETY: the module handle is that of this program, the resource is
        // named by number as `MAKEINTRESOURCE` does, and every object is
        // checked before it is used. The icon handle is released by `Icon`.
        unsafe {
            let module = GetModuleHandleW(None).ok()?;
            let handle = LoadImageW(
                Some(HINSTANCE(module.0)),
                PCWSTR(usize::from(APP_ICON) as *const u16),
                IMAGE_ICON,
                side,
                side,
                LR_DEFAULTCOLOR,
            )
            .ok()?;
            let icon = Icon(HICON(handle.0));
            let bitmap = self.factory.CreateBitmapFromHICON(icon.0).ok()?;
            let converter = self.factory.CreateFormatConverter().ok()?;
            converter
                .Initialize(
                    &bitmap,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .ok()?;
            converter.cast().ok()
        }
    }
}
