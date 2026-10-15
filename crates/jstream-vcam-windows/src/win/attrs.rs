//! `IMFAttributes` for our COM objects, forwarded to an inner store from
//! `MFCreateAttributes` (VCamSample's `CBaseAttributes`). The type needs a field
//! `attrs: IMFAttributes`. Every method runs inside [`super::dll::guard_quiet`], so no
//! panic crosses into the caller; errors such as "attribute not found" are normal here
//! and aren't logged.

macro_rules! impl_attributes {
    ($impl_ty:ty, $name:literal) => {
        impl ::windows::Win32::Media::MediaFoundation::IMFAttributes_Impl for $impl_ty {
            fn GetItem(
                &self,
                key: *const ::windows_core::GUID,
                value: *mut ::windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetItem"), || {
                    // SAFETY: forwards the caller's pointers unchanged to the inner store,
                    // which has the same contract.
                    unsafe { self.attrs.GetItem(key, Some(value)) }
                })
            }

            fn GetItemType(
                &self,
                key: *const ::windows_core::GUID,
            ) -> ::windows_core::Result<::windows::Win32::Media::MediaFoundation::MF_ATTRIBUTE_TYPE>
            {
                $crate::win::dll::guard_quiet(concat!($name, "::GetItemType"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.GetItemType(key) }
                })
            }

            fn CompareItem(
                &self,
                key: *const ::windows_core::GUID,
                value: *const ::windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
            ) -> ::windows_core::Result<::windows_core::BOOL> {
                $crate::win::dll::guard_quiet(concat!($name, "::CompareItem"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.CompareItem(key, value) }
                })
            }

            fn Compare(
                &self,
                theirs: ::windows_core::Ref<
                    ::windows::Win32::Media::MediaFoundation::IMFAttributes,
                >,
                match_type: ::windows::Win32::Media::MediaFoundation::MF_ATTRIBUTES_MATCH_TYPE,
            ) -> ::windows_core::Result<::windows_core::BOOL> {
                $crate::win::dll::guard_quiet(concat!($name, "::Compare"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.Compare(theirs.as_ref(), match_type) }
                })
            }

            fn GetUINT32(&self, key: *const ::windows_core::GUID) -> ::windows_core::Result<u32> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetUINT32"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.GetUINT32(key) }
                })
            }

            fn GetUINT64(&self, key: *const ::windows_core::GUID) -> ::windows_core::Result<u64> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetUINT64"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.GetUINT64(key) }
                })
            }

            fn GetDouble(&self, key: *const ::windows_core::GUID) -> ::windows_core::Result<f64> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetDouble"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.GetDouble(key) }
                })
            }

            fn GetGUID(
                &self,
                key: *const ::windows_core::GUID,
            ) -> ::windows_core::Result<::windows_core::GUID> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetGUID"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.GetGUID(key) }
                })
            }

            fn GetStringLength(
                &self,
                key: *const ::windows_core::GUID,
            ) -> ::windows_core::Result<u32> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetStringLength"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.GetStringLength(key) }
                })
            }

            fn GetString(
                &self,
                key: *const ::windows_core::GUID,
                value: ::windows_core::PWSTR,
                cch: u32,
                pcch: *mut u32,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetString"), || {
                    use ::windows_core::Interface as _;
                    // SAFETY: calls the inner store's own vtable entry with its own `this`
                    // and the caller's arguments unchanged (the safe wrapper would need a
                    // slice, which a null buffer can't form).
                    unsafe {
                        (self.attrs.vtable().GetString)(self.attrs.as_raw(), key, value, cch, pcch)
                            .ok()
                    }
                })
            }

            fn GetAllocatedString(
                &self,
                key: *const ::windows_core::GUID,
                value: *mut ::windows_core::PWSTR,
                pcch: *mut u32,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetAllocatedString"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.GetAllocatedString(key, value, pcch) }
                })
            }

            fn GetBlobSize(&self, key: *const ::windows_core::GUID) -> ::windows_core::Result<u32> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetBlobSize"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.GetBlobSize(key) }
                })
            }

            fn GetBlob(
                &self,
                key: *const ::windows_core::GUID,
                buf: *mut u8,
                size: u32,
                psize: *mut u32,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetBlob"), || {
                    use ::windows_core::Interface as _;
                    // SAFETY: as in GetString: the inner vtable, arguments unchanged.
                    unsafe {
                        (self.attrs.vtable().GetBlob)(self.attrs.as_raw(), key, buf, size, psize)
                            .ok()
                    }
                })
            }

            fn GetAllocatedBlob(
                &self,
                key: *const ::windows_core::GUID,
                buf: *mut *mut u8,
                psize: *mut u32,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetAllocatedBlob"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.GetAllocatedBlob(key, buf, psize) }
                })
            }

            fn GetUnknown(
                &self,
                key: *const ::windows_core::GUID,
                riid: *const ::windows_core::GUID,
                ppv: *mut *mut ::core::ffi::c_void,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetUnknown"), || {
                    use ::windows_core::Interface as _;
                    // SAFETY: as in GetString: the inner vtable, arguments unchanged.
                    unsafe {
                        (self.attrs.vtable().GetUnknown)(self.attrs.as_raw(), key, riid, ppv).ok()
                    }
                })
            }

            fn SetItem(
                &self,
                key: *const ::windows_core::GUID,
                value: *const ::windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::SetItem"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.SetItem(key, value) }
                })
            }

            fn DeleteItem(&self, key: *const ::windows_core::GUID) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::DeleteItem"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.DeleteItem(key) }
                })
            }

            fn DeleteAllItems(&self) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::DeleteAllItems"), || {
                    // SAFETY: plain call on the inner store.
                    unsafe { self.attrs.DeleteAllItems() }
                })
            }

            fn SetUINT32(
                &self,
                key: *const ::windows_core::GUID,
                value: u32,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::SetUINT32"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.SetUINT32(key, value) }
                })
            }

            fn SetUINT64(
                &self,
                key: *const ::windows_core::GUID,
                value: u64,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::SetUINT64"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.SetUINT64(key, value) }
                })
            }

            fn SetDouble(
                &self,
                key: *const ::windows_core::GUID,
                value: f64,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::SetDouble"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.SetDouble(key, value) }
                })
            }

            fn SetGUID(
                &self,
                key: *const ::windows_core::GUID,
                value: *const ::windows_core::GUID,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::SetGUID"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.SetGUID(key, value) }
                })
            }

            fn SetString(
                &self,
                key: *const ::windows_core::GUID,
                value: &::windows_core::PCWSTR,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::SetString"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.SetString(key, *value) }
                })
            }

            fn SetBlob(
                &self,
                key: *const ::windows_core::GUID,
                buf: *const u8,
                size: u32,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::SetBlob"), || {
                    use ::windows_core::Interface as _;
                    // SAFETY: as in GetString: the inner vtable, arguments unchanged.
                    unsafe {
                        (self.attrs.vtable().SetBlob)(self.attrs.as_raw(), key, buf, size).ok()
                    }
                })
            }

            fn SetUnknown(
                &self,
                key: *const ::windows_core::GUID,
                unknown: ::windows_core::Ref<::windows_core::IUnknown>,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::SetUnknown"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.SetUnknown(key, unknown.as_ref()) }
                })
            }

            fn LockStore(&self) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::LockStore"), || {
                    // SAFETY: plain call on the inner store.
                    unsafe { self.attrs.LockStore() }
                })
            }

            fn UnlockStore(&self) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::UnlockStore"), || {
                    // SAFETY: plain call on the inner store.
                    unsafe { self.attrs.UnlockStore() }
                })
            }

            fn GetCount(&self) -> ::windows_core::Result<u32> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetCount"), || {
                    // SAFETY: plain call on the inner store.
                    unsafe { self.attrs.GetCount() }
                })
            }

            fn GetItemByIndex(
                &self,
                index: u32,
                key: *mut ::windows_core::GUID,
                value: *mut ::windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::GetItemByIndex"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.GetItemByIndex(index, key, Some(value)) }
                })
            }

            fn CopyAllItems(
                &self,
                dest: ::windows_core::Ref<::windows::Win32::Media::MediaFoundation::IMFAttributes>,
            ) -> ::windows_core::Result<()> {
                $crate::win::dll::guard_quiet(concat!($name, "::CopyAllItems"), || {
                    // SAFETY: as in GetItem.
                    unsafe { self.attrs.CopyAllItems(dest.as_ref()) }
                })
            }
        }
    };
}

pub(crate) use impl_attributes;
