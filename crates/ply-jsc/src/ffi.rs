//! The slice of JavaScriptCore's C API ply uses, plus small helpers.
//!
//! Every function here must be called on the JS thread that owns the
//! context group; the runtime guarantees that by construction.

#![allow(non_camel_case_types, non_upper_case_globals, clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::ffi::c_char;
use std::ptr::{null, null_mut};

pub type JSContextGroupRef = *const c_void;
pub type JSContextRef = *const c_void;
pub type JSGlobalContextRef = *mut c_void;
pub type JSValueRef = *const c_void;
pub type JSObjectRef = *mut c_void;
pub type JSStringRef = *mut c_void;
pub type JSClassRef = *mut c_void;

/// Layout of the C API's JSClassDefinition (unused hooks stay null).
#[repr(C)]
pub struct JSClassDefinition {
    pub version: i32,
    pub attributes: u32,
    pub class_name: *const c_char,
    pub parent: JSClassRef,
    pub static_values: *const c_void,
    pub static_functions: *const c_void,
    pub initialize: *const c_void,
    pub finalize: Option<unsafe extern "C" fn(JSObjectRef)>,
    pub has_property: *const c_void,
    pub get_property: *const c_void,
    pub set_property: *const c_void,
    pub delete_property: *const c_void,
    pub get_property_names: *const c_void,
    pub call: Option<Callback>,
    pub construct: *const c_void,
    pub has_instance: *const c_void,
    pub convert: *const c_void,
}
pub type Callback = unsafe extern "C" fn(
    ctx: JSContextRef,
    function: JSObjectRef,
    this: JSObjectRef,
    argc: usize,
    argv: *const JSValueRef,
    exception: *mut JSValueRef,
) -> JSValueRef;

pub const TYPED_UINT8: u32 = 3;

/// Called when a script has run past the group's time limit; `true` ends it.
pub type ShouldTerminate = unsafe extern "C" fn(ctx: JSContextRef, context: *mut c_void) -> bool;

#[link(name = "JavaScriptCore", kind = "framework")]
unsafe extern "C" {
    pub fn JSContextGroupCreate() -> JSContextGroupRef;
    pub fn JSContextGroupRelease(group: JSContextGroupRef);
    pub fn JSGlobalContextCreateInGroup(group: JSContextGroupRef, class: JSClassRef) -> JSGlobalContextRef;
    pub fn JSGlobalContextRelease(ctx: JSGlobalContextRef);
    pub fn JSGlobalContextSetName(ctx: JSGlobalContextRef, name: JSStringRef);
    pub fn JSEvaluateScript(
        ctx: JSContextRef,
        script: JSStringRef,
        this: JSObjectRef,
        source_url: JSStringRef,
        line: i32,
        exception: *mut JSValueRef,
    ) -> JSValueRef;

    pub fn JSStringCreateWithCharacters(chars: *const u16, len: usize) -> JSStringRef;
    pub fn JSStringRelease(s: JSStringRef);
    pub fn JSStringGetLength(s: JSStringRef) -> usize;
    pub fn JSStringGetCharactersPtr(s: JSStringRef) -> *const u16;

    pub fn JSValueToStringCopy(ctx: JSContextRef, v: JSValueRef, exception: *mut JSValueRef) -> JSStringRef;
    pub fn JSValueMakeString(ctx: JSContextRef, s: JSStringRef) -> JSValueRef;
    pub fn JSValueMakeUndefined(ctx: JSContextRef) -> JSValueRef;
    pub fn JSValueMakeNumber(ctx: JSContextRef, n: f64) -> JSValueRef;
    pub fn JSValueMakeBoolean(ctx: JSContextRef, b: bool) -> JSValueRef;
    pub fn JSValueMakeFromJSONString(ctx: JSContextRef, s: JSStringRef) -> JSValueRef;
    pub fn JSValueIsString(ctx: JSContextRef, v: JSValueRef) -> bool;
    pub fn JSValueIsObject(ctx: JSContextRef, v: JSValueRef) -> bool;
    pub fn JSValueToObject(ctx: JSContextRef, v: JSValueRef, exception: *mut JSValueRef) -> JSObjectRef;
    pub fn JSValueProtect(ctx: JSContextRef, v: JSValueRef);
    pub fn JSValueUnprotect(ctx: JSContextRef, v: JSValueRef);
    pub fn JSValueGetTypedArrayType(ctx: JSContextRef, v: JSValueRef, exception: *mut JSValueRef) -> u32;

    pub fn JSObjectMakeFunctionWithCallback(ctx: JSContextRef, name: JSStringRef, cb: Callback) -> JSObjectRef;
    pub fn JSClassCreate(definition: *const JSClassDefinition) -> JSClassRef;
    pub fn JSClassRelease(class: JSClassRef);
    pub fn JSObjectMake(ctx: JSContextRef, class: JSClassRef, private: *mut c_void) -> JSObjectRef;
    pub fn JSObjectGetPrivate(object: JSObjectRef) -> *mut c_void;
    pub fn JSObjectIsFunction(ctx: JSContextRef, object: JSObjectRef) -> bool;
    pub fn JSValueToNumber(ctx: JSContextRef, value: JSValueRef, exception: *mut JSValueRef) -> f64;
    pub fn JSValueToBoolean(ctx: JSContextRef, value: JSValueRef) -> bool;
    pub fn JSObjectSetProperty(ctx: JSContextRef, object: JSObjectRef, name: JSStringRef, value: JSValueRef, attributes: u32, exception: *mut JSValueRef);
    pub fn JSObjectGetProperty(ctx: JSContextRef, obj: JSObjectRef, name: JSStringRef, exception: *mut JSValueRef) -> JSValueRef;
    pub fn JSObjectCallAsFunction(
        ctx: JSContextRef,
        f: JSObjectRef,
        this: JSObjectRef,
        argc: usize,
        argv: *const JSValueRef,
        exception: *mut JSValueRef,
    ) -> JSValueRef;
    pub fn JSObjectMakeDeferredPromise(
        ctx: JSContextRef,
        resolve: *mut JSObjectRef,
        reject: *mut JSObjectRef,
        exception: *mut JSValueRef,
    ) -> JSObjectRef;
    pub fn JSObjectMakeTypedArray(ctx: JSContextRef, kind: u32, len: usize, exception: *mut JSValueRef) -> JSObjectRef;
    pub fn JSObjectGetTypedArrayBytesPtr(ctx: JSContextRef, obj: JSObjectRef, exception: *mut JSValueRef) -> *mut c_void;
    pub fn JSObjectGetTypedArrayByteLength(ctx: JSContextRef, obj: JSObjectRef, exception: *mut JSValueRef) -> usize;

    // Test-only full, synchronous GC; the public JSGarbageCollect merely
    // schedules collection. Exported by the system framework.
    #[cfg(test)]
    pub fn JSSynchronousGarbageCollectForDebugging(ctx: JSContextRef);

    // Exported by the system framework, declared in JSC's private headers
    // (JSContextRefPrivate.h). Stable since macOS 10.6 / 12 respectively.
    pub fn JSContextGroupSetExecutionTimeLimit(
        group: JSContextGroupRef,
        limit: f64,
        callback: Option<ShouldTerminate>,
        context: *mut c_void,
    );
    pub fn JSGlobalContextSetUnhandledRejectionCallback(
        ctx: JSGlobalContextRef,
        function: JSObjectRef,
        exception: *mut JSValueRef,
    );
}

/// An owned `JSStringRef`.
pub struct JsStr(pub JSStringRef);

impl JsStr {
    pub fn new(s: &str) -> JsStr {
        let utf16: Vec<u16> = s.encode_utf16().collect();
        JsStr(unsafe { JSStringCreateWithCharacters(utf16.as_ptr(), utf16.len()) })
    }

    pub fn to_string_lossy(&self) -> String {
        unsafe {
            let len = JSStringGetLength(self.0);
            let ptr = JSStringGetCharactersPtr(self.0);
            if ptr.is_null() || len == 0 {
                return String::new();
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
        }
    }
}

impl Drop for JsStr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { JSStringRelease(self.0) }
        }
    }
}

/// A JS value kept alive while Rust owns it, including inside a heap-backed
/// collection that JSC's conservative stack scan cannot see.
///
/// Create the root before making another allocating JSC call. The context
/// must outlive the root, and both creation and drop belong on its JS thread.
pub struct RootedValue {
    ctx: JSContextRef,
    value: JSValueRef,
}

impl RootedValue {
    pub unsafe fn new(ctx: JSContextRef, value: JSValueRef) -> Self {
        unsafe { JSValueProtect(ctx, value) };
        Self { ctx, value }
    }

    pub fn get(&self) -> JSValueRef {
        self.value
    }
}

impl Drop for RootedValue {
    fn drop(&mut self) {
        unsafe { JSValueUnprotect(self.ctx, self.value) };
    }
}

pub unsafe fn string_value(ctx: JSContextRef, s: &str) -> JSValueRef {
    let js = JsStr::new(s);
    unsafe { JSValueMakeString(ctx, js.0) }
}

/// `String(value)`.
pub unsafe fn to_rust_string(ctx: JSContextRef, v: JSValueRef) -> String {
    unsafe {
        let s = JSValueToStringCopy(ctx, v, null_mut());
        if s.is_null() {
            return String::new();
        }
        JsStr(s).to_string_lossy()
    }
}

/// Parse JSON into a JS value (`undefined` if the JSON is invalid).
pub unsafe fn json_value(ctx: JSContextRef, json: &str) -> JSValueRef {
    unsafe {
        let s = JsStr::new(json);
        let v = JSValueMakeFromJSONString(ctx, s.0);
        if v.is_null() { JSValueMakeUndefined(ctx) } else { v }
    }
}

pub unsafe fn bytes_value(ctx: JSContextRef, bytes: &[u8]) -> JSValueRef {
    unsafe {
        let arr = JSObjectMakeTypedArray(ctx, TYPED_UINT8, bytes.len(), null_mut());
        if !bytes.is_empty() {
            let ptr = JSObjectGetTypedArrayBytesPtr(ctx, arr, null_mut()) as *mut u8;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        }
        arr as JSValueRef
    }
}

/// Bytes of a `Uint8Array` the prelude made with byte offset 0.
pub unsafe fn value_bytes(ctx: JSContextRef, v: JSValueRef) -> Option<Vec<u8>> {
    unsafe {
        if v.is_null() || JSValueGetTypedArrayType(ctx, v, null_mut()) != TYPED_UINT8 {
            return None;
        }
        let obj = JSValueToObject(ctx, v, null_mut());
        let len = JSObjectGetTypedArrayByteLength(ctx, obj, null_mut());
        if len == 0 {
            return Some(Vec::new());
        }
        let ptr = JSObjectGetTypedArrayBytesPtr(ctx, obj, null_mut()) as *const u8;
        Some(std::slice::from_raw_parts(ptr, len).to_vec())
    }
}

pub unsafe fn get_property(ctx: JSContextRef, obj: JSObjectRef, name: &str) -> JSValueRef {
    let n = JsStr::new(name);
    unsafe { JSObjectGetProperty(ctx, obj, n.0, null_mut()) }
}

/// Call `f(args)`; an exception comes back as its `String(...)`/stack text.
pub unsafe fn call(ctx: JSContextRef, f: JSObjectRef, args: &[JSValueRef]) -> Result<JSValueRef, String> {
    unsafe {
        if f.is_null() || !JSObjectIsFunction(ctx, f) { return Err("value is not callable".into()); }
        let mut exc: JSValueRef = null();
        let r = JSObjectCallAsFunction(ctx, f, null_mut(), args.len(), args.as_ptr(), &mut exc);
        if exc.is_null() { Ok(r) } else { Err(describe_exception(ctx, exc)) }
    }
}

pub unsafe fn eval(ctx: JSContextRef, code: &str, url: &str) -> Result<JSValueRef, String> {
    unsafe {
        let script = JsStr::new(code);
        let source = JsStr::new(url);
        let mut exc: JSValueRef = null();
        let r = JSEvaluateScript(ctx, script.0, null_mut(), source.0, 1, &mut exc);
        if exc.is_null() { Ok(r) } else { Err(describe_exception(ctx, exc)) }
    }
}

pub unsafe fn describe_exception(ctx: JSContextRef, exc: JSValueRef) -> String {
    unsafe {
        let text = to_rust_string(ctx, exc);
        if JSValueIsObject(ctx, exc) {
            let obj = JSValueToObject(ctx, exc, null_mut());
            let stack = get_property(ctx, obj, "stack");
            if !stack.is_null() && JSValueIsString(ctx, stack) {
                let stack = to_rust_string(ctx, stack);
                if !stack.is_empty() {
                    return format!("{text}\n{stack}");
                }
            }
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_on_the_rust_heap_survive_full_gc() {
        unsafe {
            let group = JSContextGroupCreate();
            let ctx = JSGlobalContextCreateInGroup(group, null_mut());
            let actual = {
                let inspect = RootedValue::new(ctx, eval(ctx, "(...args) => JSON.stringify(args)", "gc-test").unwrap());
                let mut roots = Vec::new();
                for n in 0..64 {
                    let text = format!("argument-{n}:{}", "多 agent 上下文 / ".repeat(1024));
                    roots.push(RootedValue::new(ctx, string_value(ctx, &text)));
                    // Older arguments are only in the Vec, not on the
                    // machine stack. Collect while building the call.
                    JSSynchronousGarbageCollectForDebugging(ctx);
                }
                let args: Vec<_> = roots.iter().map(RootedValue::get).collect();
                let result = call(ctx, inspect.get() as JSObjectRef, &args).unwrap();
                to_rust_string(ctx, result)
            };
            JSGlobalContextRelease(ctx);
            JSContextGroupRelease(group);
            let actual: Vec<String> = serde_json::from_str(&actual).unwrap();
            let expected: Vec<_> = (0..64).map(|n| format!("argument-{n}:{}", "多 agent 上下文 / ".repeat(1024))).collect();
            assert_eq!(actual, expected);
        }
    }
}
