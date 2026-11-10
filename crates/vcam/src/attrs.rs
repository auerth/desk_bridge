//! `IMFAttributes` forwarding: `IMFActivate` derives from it, and the activator simply keeps a
//! Media Foundation attribute store. The calls go straight through the store's vtable, so every
//! pointer the caller passed reaches the real implementation unchanged.

macro_rules! forward_attributes {
    ($ty:ty, $field:ident) => {
        impl windows::Win32::Media::MediaFoundation::IMFAttributes_Impl for $ty {
            fn GetItem(
                &self,
                k: *const windows::core::GUID,
                v: *mut windows::core::PROPVARIANT,
            ) -> windows::core::Result<()> {
                let a = &self.$field;
                unsafe {
                    (windows::core::Interface::vtable(a).GetItem)(
                        windows::core::Interface::as_raw(a),
                        k,
                        v.cast(),
                    )
                    .ok()
                }
            }
            fn GetItemType(
                &self,
                k: *const windows::core::GUID,
            ) -> windows::core::Result<windows::Win32::Media::MediaFoundation::MF_ATTRIBUTE_TYPE>
            {
                unsafe { self.$field.GetItemType(k) }
            }
            fn CompareItem(
                &self,
                k: *const windows::core::GUID,
                v: *const windows::core::PROPVARIANT,
            ) -> windows::core::Result<windows::Win32::Foundation::BOOL> {
                let a = &self.$field;
                let mut out = windows::Win32::Foundation::BOOL::default();
                unsafe {
                    (windows::core::Interface::vtable(a).CompareItem)(
                        windows::core::Interface::as_raw(a),
                        k,
                        v.cast(),
                        &mut out,
                    )
                    .ok()?
                };
                Ok(out)
            }
            fn Compare(
                &self,
                theirs: Option<&windows::Win32::Media::MediaFoundation::IMFAttributes>,
                t: windows::Win32::Media::MediaFoundation::MF_ATTRIBUTES_MATCH_TYPE,
            ) -> windows::core::Result<windows::Win32::Foundation::BOOL> {
                let a = &self.$field;
                let mut out = windows::Win32::Foundation::BOOL::default();
                let p = theirs.map_or(std::ptr::null_mut(), windows::core::Interface::as_raw);
                unsafe {
                    (windows::core::Interface::vtable(a).Compare)(
                        windows::core::Interface::as_raw(a),
                        p,
                        t,
                        &mut out,
                    )
                    .ok()?
                };
                Ok(out)
            }
            fn GetUINT32(&self, k: *const windows::core::GUID) -> windows::core::Result<u32> {
                unsafe { self.$field.GetUINT32(k) }
            }
            fn GetUINT64(&self, k: *const windows::core::GUID) -> windows::core::Result<u64> {
                unsafe { self.$field.GetUINT64(k) }
            }
            fn GetDouble(&self, k: *const windows::core::GUID) -> windows::core::Result<f64> {
                unsafe { self.$field.GetDouble(k) }
            }
            fn GetGUID(
                &self,
                k: *const windows::core::GUID,
            ) -> windows::core::Result<windows::core::GUID> {
                unsafe { self.$field.GetGUID(k) }
            }
            fn GetStringLength(&self, k: *const windows::core::GUID) -> windows::core::Result<u32> {
                unsafe { self.$field.GetStringLength(k) }
            }
            fn GetString(
                &self,
                k: *const windows::core::GUID,
                buf: windows::core::PWSTR,
                cch: u32,
                len: *mut u32,
            ) -> windows::core::Result<()> {
                let a = &self.$field;
                unsafe {
                    (windows::core::Interface::vtable(a).GetString)(
                        windows::core::Interface::as_raw(a),
                        k,
                        buf,
                        cch,
                        len,
                    )
                    .ok()
                }
            }
            fn GetAllocatedString(
                &self,
                k: *const windows::core::GUID,
                out: *mut windows::core::PWSTR,
                len: *mut u32,
            ) -> windows::core::Result<()> {
                let a = &self.$field;
                unsafe {
                    (windows::core::Interface::vtable(a).GetAllocatedString)(
                        windows::core::Interface::as_raw(a),
                        k,
                        out,
                        len,
                    )
                    .ok()
                }
            }
            fn GetBlobSize(&self, k: *const windows::core::GUID) -> windows::core::Result<u32> {
                unsafe { self.$field.GetBlobSize(k) }
            }
            fn GetBlob(
                &self,
                k: *const windows::core::GUID,
                buf: *mut u8,
                cb: u32,
                size: *mut u32,
            ) -> windows::core::Result<()> {
                let a = &self.$field;
                unsafe {
                    (windows::core::Interface::vtable(a).GetBlob)(
                        windows::core::Interface::as_raw(a),
                        k,
                        buf,
                        cb,
                        size,
                    )
                    .ok()
                }
            }
            fn GetAllocatedBlob(
                &self,
                k: *const windows::core::GUID,
                buf: *mut *mut u8,
                size: *mut u32,
            ) -> windows::core::Result<()> {
                let a = &self.$field;
                unsafe {
                    (windows::core::Interface::vtable(a).GetAllocatedBlob)(
                        windows::core::Interface::as_raw(a),
                        k,
                        buf,
                        size,
                    )
                    .ok()
                }
            }
            fn GetUnknown(
                &self,
                k: *const windows::core::GUID,
                riid: *const windows::core::GUID,
                ppv: *mut *mut core::ffi::c_void,
            ) -> windows::core::Result<()> {
                let a = &self.$field;
                unsafe {
                    (windows::core::Interface::vtable(a).GetUnknown)(
                        windows::core::Interface::as_raw(a),
                        k,
                        riid,
                        ppv,
                    )
                    .ok()
                }
            }
            fn SetItem(
                &self,
                k: *const windows::core::GUID,
                v: *const windows::core::PROPVARIANT,
            ) -> windows::core::Result<()> {
                let a = &self.$field;
                unsafe {
                    (windows::core::Interface::vtable(a).SetItem)(
                        windows::core::Interface::as_raw(a),
                        k,
                        v.cast(),
                    )
                    .ok()
                }
            }
            fn DeleteItem(&self, k: *const windows::core::GUID) -> windows::core::Result<()> {
                unsafe { self.$field.DeleteItem(k) }
            }
            fn DeleteAllItems(&self) -> windows::core::Result<()> {
                unsafe { self.$field.DeleteAllItems() }
            }
            fn SetUINT32(
                &self,
                k: *const windows::core::GUID,
                v: u32,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.SetUINT32(k, v) }
            }
            fn SetUINT64(
                &self,
                k: *const windows::core::GUID,
                v: u64,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.SetUINT64(k, v) }
            }
            fn SetDouble(
                &self,
                k: *const windows::core::GUID,
                v: f64,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.SetDouble(k, v) }
            }
            fn SetGUID(
                &self,
                k: *const windows::core::GUID,
                v: *const windows::core::GUID,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.SetGUID(k, v) }
            }
            fn SetString(
                &self,
                k: *const windows::core::GUID,
                v: &windows::core::PCWSTR,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.SetString(k, *v) }
            }
            fn SetBlob(
                &self,
                k: *const windows::core::GUID,
                buf: *const u8,
                cb: u32,
            ) -> windows::core::Result<()> {
                let a = &self.$field;
                unsafe {
                    (windows::core::Interface::vtable(a).SetBlob)(
                        windows::core::Interface::as_raw(a),
                        k,
                        buf,
                        cb,
                    )
                    .ok()
                }
            }
            fn SetUnknown(
                &self,
                k: *const windows::core::GUID,
                u: Option<&windows::core::IUnknown>,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.SetUnknown(k, u) }
            }
            fn LockStore(&self) -> windows::core::Result<()> {
                unsafe { self.$field.LockStore() }
            }
            fn UnlockStore(&self) -> windows::core::Result<()> {
                unsafe { self.$field.UnlockStore() }
            }
            fn GetCount(&self) -> windows::core::Result<u32> {
                unsafe { self.$field.GetCount() }
            }
            fn GetItemByIndex(
                &self,
                i: u32,
                k: *mut windows::core::GUID,
                v: *mut windows::core::PROPVARIANT,
            ) -> windows::core::Result<()> {
                let a = &self.$field;
                unsafe {
                    (windows::core::Interface::vtable(a).GetItemByIndex)(
                        windows::core::Interface::as_raw(a),
                        i,
                        k,
                        v.cast(),
                    )
                    .ok()
                }
            }
            fn CopyAllItems(
                &self,
                dest: Option<&windows::Win32::Media::MediaFoundation::IMFAttributes>,
            ) -> windows::core::Result<()> {
                unsafe { self.$field.CopyAllItems(dest) }
            }
        }
    };
}
pub(crate) use forward_attributes;
