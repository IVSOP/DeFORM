use std::{fmt::Display, ptr, slice, str::Utf8Error};

use serde::Serialize;

/// Owned byte range crossing the ABI boundary, pointer plus length.
///
/// Used in both directions. As an argument it borrows memory owned by the caller and is
/// only read for the duration of the call. As a return value it owns a leaked allocation
/// that the caller must hand back to [`deform_free_bytes`].
#[repr(C)]
pub struct ByteBuffer {
    pub ptr: *mut u8,
    pub size: usize,
}

impl ByteBuffer {
    pub const fn null() -> Self {
        Self {
            ptr: ptr::null_mut(),
            size: 0,
        }
    }

    pub const fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    /// # Safety
    /// `ptr` must either be null or point to `size` initialized bytes that stay valid and
    /// unwritten for the lifetime `'a` the caller picks.
    pub unsafe fn as_slice<'a>(&self) -> &'a [u8] {
        if self.ptr.is_null() {
            &[]
        } else {
            unsafe { slice::from_raw_parts(self.ptr, self.size) }
        }
    }

    /// Same as [`ByteBuffer::as_slice`], but validates UTF-8 instead of assuming it, so a
    /// mangled string from the host surfaces as an error rather than as undefined behaviour.
    ///
    /// # Safety
    /// See [`ByteBuffer::as_slice`].
    pub unsafe fn as_str<'a>(&self) -> Result<&'a str, Utf8Error> {
        std::str::from_utf8(unsafe { self.as_slice() })
    }
}

/// The envelope every fallible export returns, as JSON: `{"data": ...}` or `{"error": "..."}`.
/// Untagged so the host reads one key and knows which arm it got.
#[derive(Serialize)]
#[serde(untagged)]
enum CResult {
    Ok { data: serde_json::Value },
    Err { error: String },
}

/// Leaks `s` as a [`ByteBuffer`]. Freed with [`deform_free_bytes`].
pub fn string_to_buffer(s: String) -> ByteBuffer {
    let raw = s.leak();
    ByteBuffer {
        ptr: raw.as_mut_ptr(),
        size: raw.len(),
    }
}

/// Serializes `data` into a success envelope.
///
/// A type that fails to serialize comes back as an error envelope rather than a panic --
/// unwinding out of an `extern "C"` function is undefined behaviour.
pub fn json_data<T: Serialize>(data: &T) -> ByteBuffer {
    let value = match serde_json::to_value(data) {
        Ok(value) => value,
        Err(e) => return json_error(e),
    };

    match serde_json::to_string(&CResult::Ok { data: value }) {
        Ok(s) => string_to_buffer(s),
        Err(e) => json_error(e),
    }
}

/// Serializes `err`'s `Display` into an error envelope.
pub fn json_error(err: impl Display) -> ByteBuffer {
    let error = err.to_string();

    match serde_json::to_string(&CResult::Err {
        error: error.clone(),
    }) {
        Ok(s) => string_to_buffer(s),
        // `serde_json` only fails here on a non-string map key or a non-finite float, and
        // this value is a plain string, so this arm is unreachable in practice. Escaping by
        // hand still beats a panic across the boundary.
        Err(_) => string_to_buffer(format!(
            "{{\"error\":\"{}\"}}",
            error.replace('\\', "\\\\").replace('"', "\\\"")
        )),
    }
}

/// Collapses a `Result` into the matching envelope.
pub fn json_result<T: Serialize, E: Display>(result: Result<T, E>) -> ByteBuffer {
    match result {
        Ok(data) => json_data(&data),
        Err(e) => json_error(e),
    }
}

/// Frees a [`ByteBuffer`] returned by any of these bindings.
///
/// # Safety
/// `ptr`/`size` must be exactly what one of these bindings returned, and must not have been
/// freed already. Unlike `free`, a null `ptr` is not a no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deform_free_bytes(ptr: *mut u8, size: usize) {
    drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(ptr, size)) });
}
