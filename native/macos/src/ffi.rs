//! The only unsafe module in this crate.  Every selector and ABI signature is
//! private and fixed; callers receive typed owned objects or an explicit error.

use core::ffi::c_void;
use core::ptr::NonNull;

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ObjectRef(NonNull<c_void>);

#[cfg(target_os = "macos")]
impl ObjectRef {
    fn as_ptr(self) -> *mut c_void {
        self.0.as_ptr()
    }
}

#[cfg(test)]
pub(crate) fn test_object_ref() -> ObjectRef {
    ObjectRef(NonNull::dangling())
}

#[cfg(target_os = "macos")]
mod apple {
    use super::{NonNull, ObjectRef, OwnedObject, c_void};
    use core::ffi::c_char;
    use std::ffi::{CStr, CString};

    /// NSRect transferred by value across the ABI (indirect on arm64).
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct CNRect {
        pub x: f64,
        pub y: f64,
        pub width: f64,
        pub height: f64,
    }

    /// NSPoint transferred by value across the ABI.
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct CNPoint {
        pub x: f64,
        pub y: f64,
    }

    /// NSSize transferred by value across the ABI (register-passed on arm64).
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct CNSize {
        pub width: f64,
        pub height: f64,
    }

    #[link(name = "objc", kind = "dylib")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend(receiver: *mut c_void, selector: *mut c_void, ...) -> *mut c_void;
        fn objc_retain(object: *mut c_void) -> *mut c_void;
        fn objc_release(object: *mut c_void);
        #[link_name = "objc_msgSend"]
        fn msg_send_init_window(
            receiver: *mut c_void,
            selector: *mut c_void,
            rect: CNRect,
            style: u64,
            backing: u64,
            defer: u8,
        ) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_rect(receiver: *mut c_void, selector: *mut c_void) -> CNRect;
        #[link_name = "objc_msgSend"]
        fn msg_send_obj(receiver: *mut c_void, selector: *mut c_void) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_init_view(
            receiver: *mut c_void,
            selector: *mut c_void,
            rect: CNRect,
        ) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_clear_color(
            receiver: *mut c_void,
            selector: *mut c_void,
            r: f64,
            g: f64,
            b: f64,
            a: f64,
        ) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_obj_u64(
            receiver: *mut c_void,
            selector: *mut c_void,
            index: u64,
        ) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_two_f64(
            receiver: *mut c_void,
            selector: *mut c_void,
            first: f64,
            second: f64,
        ) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_void_obj(receiver: *mut c_void, selector: *mut c_void, value: *mut c_void);
        #[link_name = "objc_msgSend"]
        fn msg_send_void_i64(receiver: *mut c_void, selector: *mut c_void, value: i64);
        #[link_name = "objc_msgSend"]
        fn msg_send_void_size(receiver: *mut c_void, selector: *mut c_void, size: CNSize);
        #[link_name = "objc_msgSend"]
        fn msg_send_obj_cchar(
            receiver: *mut c_void,
            selector: *mut c_void,
            value: *const c_char,
        ) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_f64(receiver: *mut c_void, selector: *mut c_void) -> f64;
        #[link_name = "objc_msgSend"]
        fn msg_send_i64(receiver: *mut c_void, selector: *mut c_void) -> i64;
        #[link_name = "objc_msgSend"]
        fn msg_send_u64(receiver: *mut c_void, selector: *mut c_void) -> u64;
        #[link_name = "objc_msgSend"]
        fn msg_send_charptr(receiver: *mut c_void, selector: *mut c_void) -> *const c_char;
        #[link_name = "objc_msgSend"]
        fn msg_send_next_event(
            receiver: *mut c_void,
            selector: *mut c_void,
            mask: u64,
            until: *mut c_void,
            mode: *mut c_void,
            dequeue: u8,
        ) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_key_event(
            receiver: *mut c_void,
            selector: *mut c_void,
            event_type: u64,
            location: CNPoint,
            timestamp: f64,
            window_number: i64,
            context: *mut c_void,
            modifier_flags: u64,
            characters: *mut c_void,
            ignoring: *mut c_void,
            repeat: u8,
            key_code: i64,
        ) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_scroll_event(
            receiver: *mut c_void,
            selector: *mut c_void,
            delta_x: f64,
            delta_y: f64,
            delta_z: f64,
            modifiers: i64,
            timestamp: f64,
            window_number: i64,
            context: *mut c_void,
            units_per_line: f64,
            padding: u8,
        ) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_post_event(
            receiver: *mut c_void,
            selector: *mut c_void,
            event: *mut c_void,
            at_start: u8,
        );
        #[link_name = "objc_msgSend"]
        fn msg_send_void(receiver: *mut c_void, selector: *mut c_void);
        #[link_name = "objc_msgSend"]
        fn msg_send_void_u64(receiver: *mut c_void, selector: *mut c_void, value: u64);
    }

    #[link(name = "AppKit", kind = "framework")]
    #[link(name = "QuartzCore", kind = "framework")]
    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        fn MTLCreateSystemDefaultDevice() -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_new_buffer(
            receiver: *mut c_void,
            selector: *mut c_void,
            length: u64,
            options: u64,
        ) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_contents(receiver: *mut c_void, selector: *mut c_void) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_buffer_length(receiver: *mut c_void, selector: *mut c_void) -> u64;
        #[link_name = "objc_msgSend"]
        fn msg_send_new_queue(receiver: *mut c_void, selector: *mut c_void) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_command_buffer(receiver: *mut c_void, selector: *mut c_void) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_blit_encoder(receiver: *mut c_void, selector: *mut c_void) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_copy_bytes(
            encoder: *mut c_void,
            selector: *mut c_void,
            src: *mut c_void,
            src_off: u64,
            dst: *mut c_void,
            dst_off: u64,
            size: u64,
        );
        #[link_name = "objc_msgSend"]
        fn msg_send_end_encoding(encoder: *mut c_void, selector: *mut c_void);
        #[link_name = "objc_msgSend"]
        fn msg_send_commit(buffer: *mut c_void, selector: *mut c_void);
        #[link_name = "objc_msgSend"]
        fn msg_send_wait(buffer: *mut c_void, selector: *mut c_void);
        #[link_name = "objc_msgSend"]
        fn msg_send_void_f64(receiver: *mut c_void, selector: *mut c_void, value: f64);
        #[link_name = "objc_msgSend"]
        fn msg_send_obj_obj(
            receiver: *mut c_void,
            selector: *mut c_void,
            val: *mut c_void,
        ) -> *mut c_void;
        #[link_name = "objc_msgSend"]
        fn msg_send_void_obj_obj(
            receiver: *mut c_void,
            selector: *mut c_void,
            val1: *mut c_void,
            val2: *mut c_void,
        );
    }

    #[link(name = "System", kind = "dylib")]
    unsafe extern "C" {
        fn pthread_main_np() -> i32;
    }

    fn class(name: &'static [u8]) -> Option<*mut c_void> {
        let value = unsafe { objc_getClass(name.as_ptr().cast()) };
        (!value.is_null()).then_some(value)
    }

    fn selector(name: &'static [u8]) -> Option<*mut c_void> {
        let value = unsafe { sel_registerName(name.as_ptr().cast()) };
        (!value.is_null()).then_some(value)
    }

    fn retain(value: *mut c_void) -> Option<ObjectRef> {
        let value = NonNull::new(value)?;
        let retained = unsafe { objc_retain(value.as_ptr()) };
        NonNull::new(retained).map(ObjectRef)
    }

    pub(crate) fn adopt_owned(value: *mut c_void) -> Option<ObjectRef> {
        // MTLCreateSystemDefaultDevice is NS_RETURNS_RETAINED in the selected
        // Apple SDK. Its +1 is adopted directly; retaining here would leak.
        NonNull::new(value).map(ObjectRef)
    }

    pub(crate) fn is_main_thread() -> bool {
        unsafe { pthread_main_np() != 0 }
    }

    pub(crate) fn shared_application() -> Option<ObjectRef> {
        let receiver = class(b"NSApplication\0")?;
        let message = selector(b"sharedApplication\0")?;
        let value = unsafe { objc_msgSend(receiver, message) };
        retain(value)
    }

    pub(crate) fn new_metal_layer() -> Option<ObjectRef> {
        let receiver = class(b"CAMetalLayer\0")?;
        let message = selector(b"layer\0")?;
        let value = unsafe { objc_msgSend(receiver, message) };
        retain(value)
    }

    pub(crate) fn default_metal_device() -> Option<ObjectRef> {
        let value = unsafe { MTLCreateSystemDefaultDevice() };
        adopt_owned(value)
    }
    pub(crate) fn new_buffer_with_length(device: ObjectRef, length: u64) -> Option<ObjectRef> {
        let message = selector(b"newBufferWithLength:options:\0")?;
        let value = unsafe { msg_send_new_buffer(device.as_ptr(), message, length, 0) };
        adopt_owned(value)
    }

    pub(crate) fn buffer_contents(buffer: ObjectRef) -> *mut u8 {
        let Some(message) = selector(b"contents\0") else {
            return core::ptr::null_mut();
        };
        let value = unsafe { msg_send_contents(buffer.as_ptr(), message) };
        value.cast()
    }

    #[allow(dead_code)]
    pub(crate) fn buffer_length(buffer: ObjectRef) -> u64 {
        let Some(message) = selector(b"length\0") else {
            return 0;
        };
        unsafe { msg_send_buffer_length(buffer.as_ptr(), message) }
    }

    pub(crate) fn new_command_queue(device: ObjectRef) -> Option<ObjectRef> {
        let message = selector(b"newCommandQueue\0")?;
        let value = unsafe { msg_send_new_queue(device.as_ptr(), message) };
        adopt_owned(value)
    }

    pub(crate) fn command_buffer(queue: ObjectRef) -> Option<ObjectRef> {
        let message = selector(b"commandBuffer\0")?;
        let value = unsafe { msg_send_command_buffer(queue.as_ptr(), message) };
        adopt_owned(value)
    }

    pub(crate) fn blit_encoder(cmd_buf: ObjectRef) -> Option<ObjectRef> {
        let message = selector(b"blitCommandEncoder\0")?;
        let value = unsafe { msg_send_blit_encoder(cmd_buf.as_ptr(), message) };
        adopt_owned(value)
    }

    pub(crate) fn copy_bytes(
        encoder: ObjectRef,
        src: ObjectRef,
        src_off: u64,
        dst: ObjectRef,
        dst_off: u64,
        size: u64,
    ) {
        let Some(message) =
            selector(b"copyFromBuffer:sourceOffset:toBuffer:destinationOffset:size:\0")
        else {
            return;
        };
        unsafe {
            msg_send_copy_bytes(
                encoder.as_ptr(),
                message,
                src.as_ptr(),
                src_off,
                dst.as_ptr(),
                dst_off,
                size,
            );
        }
    }

    pub(crate) fn end_encoding(encoder: ObjectRef) {
        let Some(message) = selector(b"endEncoding\0") else {
            return;
        };
        unsafe { msg_send_end_encoding(encoder.as_ptr(), message) };
    }

    pub(crate) fn commit(cmd_buf: ObjectRef) {
        let Some(message) = selector(b"commit\0") else {
            return;
        };
        unsafe { msg_send_commit(cmd_buf.as_ptr(), message) };
    }

    pub(crate) fn wait_until_completed(cmd_buf: ObjectRef) {
        let Some(message) = selector(b"waitUntilCompleted\0") else {
            return;
        };
        unsafe { msg_send_wait(cmd_buf.as_ptr(), message) };
    }

    pub(crate) fn command_buffer_status(cmd_buf: ObjectRef) -> u64 {
        let Some(message) = selector(b"status\0") else {
            return 0;
        };
        unsafe { msg_send_u64(cmd_buf.as_ptr(), message) }
    }

    pub(crate) fn owned_nsstring(text: &str) -> Option<ObjectRef> {
        let text = CString::new(text).ok()?;
        let receiver = class(b"NSString\0")?;
        let message = selector(b"stringWithUTF8String:\0")?;
        let value = unsafe { msg_send_obj_cchar(receiver, message, text.as_ptr()) };
        retain(value)
    }

    pub(crate) fn create_window(
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        style: u64,
        title: &str,
    ) -> Option<ObjectRef> {
        let title = owned_nsstring(title)?;
        let receiver = class(b"NSWindow\0")?;
        let alloc = selector(b"alloc\0")?;
        let init = selector(b"initWithContentRect:styleMask:backing:defer:\0")?;
        let raw_alloc = unsafe { objc_msgSend(receiver, alloc) };
        if raw_alloc.is_null() {
            return None;
        }
        // NSBackingStoreBuffered is the only supported backing store type.
        let value = unsafe {
            msg_send_init_window(
                raw_alloc,
                init,
                CNRect {
                    x,
                    y,
                    width,
                    height,
                },
                style,
                2,
                0,
            )
        };
        if value != raw_alloc && !raw_alloc.is_null() {
            unsafe { objc_release(raw_alloc) };
        }
        let window = adopt_owned(value)?;
        let Some(set_title) = selector(b"setTitle:\0") else {
            return Some(window);
        };
        unsafe { msg_send_void_obj(window.as_ptr(), set_title, title.as_ptr()) };
        Some(window)
    }

    pub(crate) fn set_activation_policy_accessory() -> bool {
        set_activation_policy(1)
    }

    /// NSApplicationActivationPolicyRegular = 0: Dock icon, Cmd-Tab entry,
    /// full foreground-app behavior. What a real application uses.
    pub(crate) fn set_activation_policy_regular() -> bool {
        set_activation_policy(0)
    }

    fn set_activation_policy(policy: i64) -> bool {
        let Some(app) = shared_application() else {
            return false;
        };
        let Some(message) = selector(b"setActivationPolicy:\0") else {
            return false;
        };
        unsafe { msg_send_void_i64(app.as_ptr(), message, policy) };
        true
    }

    /// Centers the window on its screen (NSWindow center).
    pub(crate) fn center_window(window: ObjectRef) {
        if let Some(message) = selector(b"center\0") {
            unsafe { msg_send_void_obj(window.as_ptr(), message, core::ptr::null_mut()) };
        }
    }

    pub(crate) fn make_key_and_order_front(window: ObjectRef) {
        let Some(message) = selector(b"makeKeyAndOrderFront:\0") else {
            return;
        };
        unsafe { msg_send_void_obj(window.as_ptr(), message, core::ptr::null_mut()) };
    }

    /// Installs a scrollable read-only monospaced text view carrying
    /// `text` as the window content, pins the window to every Space
    /// (visible above fullscreen apps), and activates the host. Demo and
    /// host-tool affordance only; the product text route stays the Metal
    /// glyph pipeline.
    pub(crate) fn install_source_text(window: ObjectRef, text: &str) -> bool {
        let Some(payload) = owned_nsstring(text) else {
            return false;
        };
        let Some(font_cls) = class(b"NSFont\0") else {
            return false;
        };
        let Some(font_sel) = selector(b"monospacedSystemFontOfSize:weight:\0") else {
            return false;
        };
        // NSFontWeightRegular is 0.0.
        let font = unsafe { msg_send_two_f64(font_cls, font_sel, 13.0_f64, 0.0_f64) };
        if font.is_null() {
            return false;
        }
        let Some(scroll_cls) = class(b"NSScrollView\0") else {
            return false;
        };
        let Some(alloc) = selector(b"alloc\0") else {
            return false;
        };
        let Some(init_frame) = selector(b"initWithFrame:\0") else {
            return false;
        };
        let scroll_raw = unsafe { objc_msgSend(scroll_cls, alloc) };
        if scroll_raw.is_null() {
            return false;
        }
        let full = CNRect {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let scroll = unsafe { msg_send_init_view(scroll_raw, init_frame, full) };
        if scroll != scroll_raw {
            unsafe { objc_release(scroll_raw) };
        }
        if scroll.is_null() {
            return false;
        }
        if let Some(scroller) = selector(b"setHasVerticalScroller:\0") {
            unsafe { msg_send_void_i64(scroll, scroller, 1) };
        }
        let Some(text_cls) = class(b"NSTextView\0") else {
            return false;
        };
        let text_raw = unsafe { objc_msgSend(text_cls, alloc) };
        if text_raw.is_null() {
            return false;
        }
        let view = unsafe { msg_send_init_view(text_raw, init_frame, full) };
        if view != text_raw {
            unsafe { objc_release(text_raw) };
        }
        if view.is_null() {
            return false;
        }
        if let Some(set_editable) = selector(b"setEditable:\0") {
            unsafe { msg_send_void_i64(view, set_editable, 0) };
        }
        if let Some(set_rich) = selector(b"setRichText:\0") {
            unsafe { msg_send_void_i64(view, set_rich, 0) };
        }
        if let Some(set_font) = selector(b"setFont:\0") {
            unsafe { msg_send_void_obj(view, set_font, font) };
        }
        if let Some(set_string) = selector(b"setString:\0") {
            unsafe { msg_send_void_obj(view, set_string, payload.as_ptr()) };
        }
        // Width+height sizing so the document view tracks the scroll view.
        if let Some(set_mask) = selector(b"setAutoresizingMask:\0") {
            unsafe { msg_send_void_i64(view, set_mask, 2 | 16) };
        }
        if let Some(set_doc) = selector(b"setDocumentView:\0") {
            unsafe { msg_send_void_obj(scroll, set_doc, view) };
        }
        if let Some(set_content) = selector(b"setContentView:\0") {
            unsafe { msg_send_void_obj(window.as_ptr(), set_content, scroll) };
        }
        // NSWindowCollectionBehaviorCanJoinAllSpaces (1 << 0) and
        // .fullScreenAuxiliary (1 << 8): the window stays visible above
        // active fullscreen Spaces instead of hiding on the desktop Space.
        if let Some(set_behavior) = selector(b"setCollectionBehavior:\0") {
            unsafe { msg_send_void_i64(window.as_ptr(), set_behavior, 1 | (1 << 8)) };
        }
        if let Some(app) = shared_application() {
            if let Some(activate) = selector(b"activateIgnoringOtherApps:\0") {
                unsafe { msg_send_void_i64(app.as_ptr(), activate, 1) };
            }
        }
        // Force the display cycle: programmatically installed views do not
        // reliably receive a draw pass in a manual event pump.
        if let Some(needs) = selector(b"setNeedsDisplay:\0") {
            unsafe { msg_send_void_i64(view, needs, 1) };
            unsafe { msg_send_void_i64(scroll, needs, 1) };
        }
        finish_launching();
        flush_core_animation();
        true
    }

    /// Marks the host application as launched. A manual event pump never
    /// runs [NSApp run], so the launch transition must be completed
    /// explicitly before AppKit services view drawing.
    pub(crate) fn finish_launching() {
        let Some(app) = shared_application() else {
            return;
        };
        if let Some(finish) = selector(b"finishLaunching\0") {
            unsafe { msg_send_void_obj(app.as_ptr(), finish, core::ptr::null_mut()) };
        }
    }

    /// Hands the main thread to NSApplication's run loop. This is the full
    /// platform event/draw loop — every automatic service AppKit performs
    /// (layout, display, deferred updates) runs from here. Exits when the
    /// app terminates (last window closes with the demo's terminate policy).
    pub(crate) fn app_run() {
        let Some(app) = shared_application() else {
            return;
        };
        if let Some(run) = selector(b"run\0") {
            unsafe { msg_send_void_obj(app.as_ptr(), run, core::ptr::null_mut()) };
        }
    }

    /// Flushes pending window updates (needsDisplay passes) for every
    /// window — the manual-pump equivalent of the runloop's automatic
    /// update servicing.
    pub(crate) fn update_windows() {
        let Some(app) = shared_application() else {
            return;
        };
        if let Some(update) = selector(b"updateWindows\0") {
            unsafe { msg_send_void_obj(app.as_ptr(), update, core::ptr::null_mut()) };
        }
    }

    /// Commits pending Core Animation state so programmatic drawing made
    /// outside a running runloop actually reaches the window server. The
    /// manual event pump never reaches the runloop's before-waiting phase,
    /// which is where the implicit transaction would otherwise commit.
    pub(crate) fn flush_core_animation() {
        let Some(cls) = class(b"CATransaction\0") else {
            return;
        };
        if let Some(commit) = selector(b"commit\0") {
            unsafe { msg_send_void_obj(cls, commit, core::ptr::null_mut()) };
        }
        if let Some(flush) = selector(b"flush\0") {
            unsafe { msg_send_void_obj(cls, flush, core::ptr::null_mut()) };
        }
    }

    pub(crate) fn window_backing_scale(window: ObjectRef) -> f64 {
        let Some(message) = selector(b"backingScaleFactor\0") else {
            return 0.0;
        };
        unsafe { msg_send_f64(window.as_ptr(), message) }
    }

    pub(crate) fn set_content_size(window: ObjectRef, width: f64, height: f64) {
        let Some(message) = selector(b"setContentSize:\0") else {
            return;
        };
        unsafe { msg_send_void_size(window.as_ptr(), message, CNSize { width, height }) };
    }

    pub(crate) fn shared_application_owned() -> Option<OwnedObject> {
        Some(OwnedObject::from_ref(shared_application()?))
    }

    pub(crate) fn next_event_polled(app: ObjectRef) -> Option<OwnedObject> {
        next_event(app, false)
    }

    /// Blocks the main thread until the next event arrives. The manual-loop
    /// equivalent of the runloop's wait: no busy polling, and AppKit services
    /// its normal per-event bookkeeping on wake.
    pub(crate) fn next_event_blocking(app: ObjectRef) -> Option<OwnedObject> {
        next_event(app, true)
    }

    fn next_event(app: ObjectRef, blocking: bool) -> Option<OwnedObject> {
        let Some(message) = selector(b"nextEventMatchingMask:untilDate:inMode:dequeue:\0") else {
            return None;
        };
        // NSAnyEventMask: the bounded pump decodes and queues every type.
        let mask = u64::MAX;
        let until = {
            let receiver = class(b"NSDate\0")?;
            let selector_name = if blocking {
                "distantFuture\0"
            } else {
                "distantPast\0"
            };
            let distant = selector(selector_name.as_bytes())?;
            let value = unsafe { objc_msgSend(receiver, distant) };
            retain(value)?
        };
        let Some(mode) = owned_nsstring("kCFRunLoopDefaultMode") else {
            return None;
        };
        let value = unsafe {
            msg_send_next_event(
                app.as_ptr(),
                message,
                mask,
                until.as_ptr(),
                mode.as_ptr(),
                1,
            )
        };
        // The returned event is autoreleased; retaining takes ownership so the
        // caller's OwnedObject release is balanced.
        retain(value).map(OwnedObject::from_ref)
    }

    /// Dispatches one platform event through the application's responder
    /// chain. The canonical manual loop is nextEvent -> sendEvent ->
    /// updateWindows; skipping sendEvent starves views of real interaction.
    pub(crate) fn send_event(app: ObjectRef, event: ObjectRef) -> bool {
        let Some(message) = selector(b"sendEvent:\0") else {
            return false;
        };
        unsafe { msg_send_void_obj(app.as_ptr(), message, event.as_ptr()) };
        true
    }

    pub(crate) fn event_raw_type(event: ObjectRef) -> u64 {
        let Some(message) = selector(b"type\0") else {
            return 0;
        };
        let value = unsafe { msg_send_i64(event.as_ptr(), message) };
        u64::try_from(value).unwrap_or(0)
    }

    pub(crate) fn event_key_code(event: ObjectRef) -> u16 {
        let Some(message) = selector(b"keyCode\0") else {
            return 0;
        };
        let value = unsafe { msg_send_i64(event.as_ptr(), message) };
        u16::try_from(value).unwrap_or(0)
    }

    pub(crate) fn event_modifier_flags(event: ObjectRef) -> u64 {
        let Some(message) = selector(b"modifierFlags\0") else {
            return 0;
        };
        unsafe { msg_send_u64(event.as_ptr(), message) }
    }

    pub(crate) fn event_is_repeat(event: ObjectRef) -> bool {
        let Some(message) = selector(b"isARepeat\0") else {
            return false;
        };
        unsafe { msg_send_i64(event.as_ptr(), message) != 0 }
    }

    pub(crate) fn event_characters_utf8(event: ObjectRef) -> Option<String> {
        let Some(message) = selector(b"characters\0") else {
            return None;
        };
        let value = unsafe { objc_msgSend(event.as_ptr(), message) };
        let characters = retain(value)?;
        let Some(utf8) = selector(b"UTF8String\0") else {
            return None;
        };
        let pointer = unsafe { msg_send_charptr(characters.as_ptr(), utf8) };
        if pointer.is_null() {
            return None;
        }
        // SAFETY: UTF8String returns a NUL-terminated buffer owned by the
        // string, which the retained handle keeps alive for this read.
        Some(
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    pub(crate) fn event_scroll_deltas(event: ObjectRef) -> (f64, f64) {
        let Some(x) = selector(b"scrollingDeltaX\0") else {
            return (0.0, 0.0);
        };
        let Some(y) = selector(b"scrollingDeltaY\0") else {
            return (0.0, 0.0);
        };
        unsafe {
            (
                msg_send_f64(event.as_ptr(), x),
                msg_send_f64(event.as_ptr(), y),
            )
        }
    }

    pub(crate) fn event_has_precise_deltas(event: ObjectRef) -> bool {
        let Some(message) = selector(b"hasPreciseScrollingDeltas\0") else {
            return false;
        };
        unsafe { msg_send_i64(event.as_ptr(), message) != 0 }
    }

    pub(crate) fn event_momentum_phase(event: ObjectRef) -> u64 {
        let Some(message) = selector(b"momentumPhase\0") else {
            return 0;
        };
        unsafe { msg_send_u64(event.as_ptr(), message) }
    }

    pub(crate) fn synthesize_key_event(
        key_down: bool,
        key_code: u16,
        characters: &str,
        modifier_flags: u64,
        repeat: bool,
    ) -> Option<OwnedObject> {
        let receiver = class(b"NSEvent\0")?;
        let message = selector(
            b"keyEventWithType:location:timestamp:windowNumber:context:modifierFlags:characters:charactersIgnoringModifiers:isARepeat:keyCode:\0",
        )?;
        let chars = owned_nsstring(characters)?;
        let ignoring = owned_nsstring(characters)?;
        let event_type: u64 = if key_down { 10 } else { 11 };
        let value = unsafe {
            msg_send_key_event(
                receiver,
                message,
                event_type,
                CNPoint { x: 0.0, y: 0.0 },
                0.0,
                0,
                core::ptr::null_mut(),
                modifier_flags,
                chars.as_ptr(),
                ignoring.as_ptr(),
                u8::from(repeat),
                i64::from(key_code),
            )
        };
        retain(value).map(OwnedObject::from_ref)
    }

    pub(crate) fn synthesize_scroll_event(
        delta_x: f64,
        delta_y: f64,
        precise: bool,
    ) -> Option<OwnedObject> {
        let receiver = class(b"NSEvent\0")?;
        let message = selector(
            b"scrollWheelEventWithDeltaX:deltaY:deltaZ:modifiers:timestamp:windowNumber:context:unitsPerLine:padding:\0",
        )?;
        let value = unsafe {
            msg_send_scroll_event(
                receiver,
                message,
                delta_x,
                delta_y,
                0.0,
                0,
                0.0,
                0,
                core::ptr::null_mut(),
                0.0,
                u8::from(precise),
            )
        };
        retain(value).map(OwnedObject::from_ref)
    }

    pub(crate) fn post_event(app: ObjectRef, event: ObjectRef, at_start: bool) -> bool {
        let Some(message) = selector(b"postEvent:atStart:\0") else {
            return false;
        };
        // postEvent:atStart: has no return value; it fails only by raising,
        // which this crate's exception policy does not contain.
        unsafe {
            msg_send_post_event(app.as_ptr(), message, event.as_ptr(), u8::from(at_start));
        }
        true
    }

    pub(crate) fn retain_object(object: ObjectRef) -> Option<ObjectRef> {
        retain(object.as_ptr())
    }

    pub(crate) fn release_object(object: ObjectRef) {
        unsafe { objc_release(object.as_ptr()) };
    }

    pub(crate) fn attach_layer_to_window(window: ObjectRef, layer: ObjectRef) -> bool {
        let Some(content_view_sel) = selector(b"contentView\0") else {
            return false;
        };
        let content_view = unsafe { objc_msgSend(window.as_ptr(), content_view_sel) };
        if content_view.is_null() {
            return false;
        }
        let Some(set_wants_layer_sel) = selector(b"setWantsLayer:\0") else {
            return false;
        };
        let Some(set_layer_sel) = selector(b"setLayer:\0") else {
            return false;
        };
        unsafe {
            msg_send_void_i64(content_view, set_wants_layer_sel, 1);
            msg_send_void_obj(content_view, set_layer_sel, layer.as_ptr());
        }
        true
    }

    pub(crate) fn detach_layer_from_window(window: ObjectRef) -> bool {
        let Some(content_view_sel) = selector(b"contentView\0") else {
            return false;
        };
        let content_view = unsafe { objc_msgSend(window.as_ptr(), content_view_sel) };
        if content_view.is_null() {
            return false;
        }
        let Some(set_layer_sel) = selector(b"setLayer:\0") else {
            return false;
        };
        unsafe {
            msg_send_void_obj(content_view, set_layer_sel, core::ptr::null_mut());
        }
        true
    }

    pub(crate) fn configure_metal_layer(
        layer: ObjectRef,
        device: ObjectRef,
        pixel_format: u64,
        width: f64,
        height: f64,
        contents_scale: f64,
    ) {
        if let Some(set_device) = selector(b"setDevice:\0") {
            unsafe { msg_send_void_obj(layer.as_ptr(), set_device, device.as_ptr()) };
        }
        if let Some(set_format) = selector(b"setPixelFormat:\0") {
            unsafe { msg_send_void_u64(layer.as_ptr(), set_format, pixel_format) };
        }
        if let Some(set_size) = selector(b"setDrawableSize:\0") {
            unsafe { msg_send_void_size(layer.as_ptr(), set_size, CNSize { width, height }) };
        }
        if let Some(set_scale) = selector(b"setContentsScale:\0") {
            unsafe { msg_send_void_f64(layer.as_ptr(), set_scale, contents_scale) };
        }
    }

    pub(crate) fn set_metal_layer_drawable_size(layer: ObjectRef, width: f64, height: f64) {
        if let Some(set_size) = selector(b"setDrawableSize:\0") {
            unsafe { msg_send_void_size(layer.as_ptr(), set_size, CNSize { width, height }) };
        }
    }

    pub(crate) fn next_drawable(layer: ObjectRef) -> Option<ObjectRef> {
        let message = selector(b"nextDrawable\0")?;
        let value = unsafe { objc_msgSend(layer.as_ptr(), message) };
        retain(value)
    }

    pub(crate) fn present_drawable(drawable: ObjectRef) {
        let Some(message) = selector(b"present\0") else {
            return;
        };
        unsafe { msg_send_void(drawable.as_ptr(), message) };
    }

    pub(crate) fn render_pass_descriptor() -> Option<ObjectRef> {
        let receiver = class(b"MTLRenderPassDescriptor\0")?;
        let message = selector(b"renderPassDescriptor\0")?;
        let value = unsafe { objc_msgSend(receiver, message) };
        retain(value)
    }

    pub(crate) fn color_attachments(rpd: ObjectRef) -> Option<ObjectRef> {
        let message = selector(b"colorAttachments\0")?;
        let value = unsafe { objc_msgSend(rpd.as_ptr(), message) };
        retain(value)
    }

    pub(crate) fn color_attachment_at(attachments: ObjectRef, index: u64) -> Option<ObjectRef> {
        let message = selector(b"objectAtIndexedSubscript:\0")?;
        let value = unsafe { msg_send_obj_u64(attachments.as_ptr(), message, index) };
        retain(value)
    }
    pub(crate) fn render_command_encoder(cmd_buf: ObjectRef, rpd: ObjectRef) -> Option<ObjectRef> {
        let message = selector(b"renderCommandEncoderWithDescriptor:\0")?;
        let value = unsafe { msg_send_obj_obj(cmd_buf.as_ptr(), message, rpd.as_ptr()) };
        retain(value)
    }

    pub(crate) fn set_attachment_texture(attachment: ObjectRef, texture: ObjectRef) {
        if let Some(message) = selector(b"setTexture:\0") {
            unsafe { msg_send_void_obj(attachment.as_ptr(), message, texture.as_ptr()) };
        }
    }

    /// MTLLoadActionClear is 2.
    pub(crate) fn set_attachment_load_clear(attachment: ObjectRef) {
        if let Some(message) = selector(b"setLoadAction:\0") {
            unsafe { msg_send_void_u64(attachment.as_ptr(), message, 2) };
        }
    }

    /// MTLStoreActionStore is 1.
    pub(crate) fn set_attachment_store(attachment: ObjectRef) {
        if let Some(message) = selector(b"setStoreAction:\0") {
            unsafe { msg_send_void_u64(attachment.as_ptr(), message, 1) };
        }
    }

    /// MTLClearColor is a four-double struct: it crosses through the typed
    /// non-variadic wrapper, never through Rust `...` (arm64 divergence).
    pub(crate) fn set_attachment_clear_color(attachment: ObjectRef, color: [f64; 4]) {
        let Some(message) = selector(b"setClearColor:\0") else {
            return;
        };
        unsafe {
            msg_send_clear_color(
                attachment.as_ptr(),
                message,
                color[0],
                color[1],
                color[2],
                color[3],
            )
        };
    }

    pub(crate) fn drawable_texture(drawable: ObjectRef) -> Option<ObjectRef> {
        let message = selector(b"texture\0")?;
        let value = unsafe { objc_msgSend(drawable.as_ptr(), message) };
        retain(value)
    }
    pub(crate) fn present_drawable_on_buffer(cmd_buf: ObjectRef, drawable: ObjectRef) {
        if let Some(message) = selector(b"presentDrawable:\0") {
            unsafe { msg_send_void_obj(cmd_buf.as_ptr(), message, drawable.as_ptr()) };
        }
    }

    pub(crate) fn display_link_available() -> bool {
        class(b"CAMetalDisplayLink\0").is_some()
    }

    pub(crate) fn create_metal_display_link(layer: ObjectRef) -> Option<OwnedObject> {
        let receiver = class(b"CAMetalDisplayLink\0")?;
        let alloc_sel = selector(b"alloc\0")?;
        let init_sel = selector(b"initWithMetalLayer:\0")?;
        unsafe {
            let allocated = objc_msgSend(receiver, alloc_sel);
            if allocated.is_null() {
                return None;
            }
            let initialized = msg_send_obj_obj(allocated, init_sel, layer.as_ptr());
            NonNull::new(initialized)
                .map(ObjectRef)
                .map(OwnedObject::from_ref)
        }
    }

    pub(crate) fn display_link_add_to_current_run_loop(link: ObjectRef) -> bool {
        let Some(run_loop_class) = class(b"NSRunLoop\0") else {
            return false;
        };
        let Some(current_run_loop_sel) = selector(b"currentRunLoop\0") else {
            return false;
        };
        let Some(add_sel) = selector(b"addToRunLoop:forMode:\0") else {
            return false;
        };
        let Some(mode) = owned_nsstring("kCFRunLoopDefaultMode") else {
            return false;
        };
        unsafe {
            let current = objc_msgSend(run_loop_class, current_run_loop_sel);
            if current.is_null() {
                return false;
            }
            msg_send_void_obj_obj(link.as_ptr(), add_sel, current, mode.as_ptr());
        }
        true
    }

    pub(crate) fn display_link_invalidate(link: ObjectRef) {
        if let Some(inv_sel) = selector(b"invalidate\0") {
            unsafe { msg_send_void(link.as_ptr(), inv_sel) };
        }
    }

    pub(crate) fn display_link_set_paused(link: ObjectRef, paused: bool) {
        if let Some(set_paused_sel) = selector(b"setPaused:\0") {
            unsafe { msg_send_void_i64(link.as_ptr(), set_paused_sel, if paused { 1 } else { 0 }) };
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod apple {
    use super::{ObjectRef, OwnedObject};

    pub(crate) fn is_main_thread() -> bool {
        true
    }

    pub(crate) fn shared_application() -> Option<ObjectRef> {
        None
    }

    pub(crate) fn new_metal_layer() -> Option<ObjectRef> {
        None
    }

    pub(crate) fn default_metal_device() -> Option<ObjectRef> {
        None
    }

    pub(crate) fn retain_object(_object: ObjectRef) -> Option<ObjectRef> {
        None
    }

    pub(crate) fn release_object(_object: ObjectRef) {}

    pub(crate) fn owned_nsstring(_text: &str) -> Option<ObjectRef> {
        None
    }

    pub(crate) fn create_window(
        _x: f64,
        _y: f64,
        _width: f64,
        _height: f64,
        _style: u64,
        _title: &str,
    ) -> Option<ObjectRef> {
        None
    }

    pub(crate) fn set_activation_policy_accessory() -> bool {
        false
    }

    pub(crate) fn make_key_and_order_front(_window: ObjectRef) {}

    pub(crate) fn window_backing_scale(_window: ObjectRef) -> f64 {
        1.0
    }

    pub(crate) fn set_content_size(_window: ObjectRef, _width: f64, _height: f64) {}

    pub(crate) fn shared_application_owned() -> Option<OwnedObject> {
        None
    }

    pub(crate) fn next_event_polled(_app: ObjectRef) -> Option<OwnedObject> {
        None
    }

    pub(crate) fn event_raw_type(_event: ObjectRef) -> u64 {
        0
    }

    pub(crate) fn event_key_code(_event: ObjectRef) -> u16 {
        0
    }

    pub(crate) fn event_modifier_flags(_event: ObjectRef) -> u64 {
        0
    }

    pub(crate) fn event_is_repeat(_event: ObjectRef) -> bool {
        false
    }

    pub(crate) fn event_characters_utf8(_event: ObjectRef) -> Option<String> {
        None
    }

    pub(crate) fn event_scroll_deltas(_event: ObjectRef) -> (f64, f64) {
        (0.0, 0.0)
    }

    pub(crate) fn event_has_precise_deltas(_event: ObjectRef) -> bool {
        false
    }

    pub(crate) fn event_momentum_phase(_event: ObjectRef) -> u64 {
        0
    }

    pub(crate) fn synthesize_key_event(
        _key_down: bool,
        _key_code: u16,
        _characters: &str,
        _modifier_flags: u64,
        _repeat: bool,
    ) -> Option<OwnedObject> {
        None
    }

    pub(crate) fn synthesize_scroll_event(
        _delta_x: f64,
        _delta_y: f64,
        _precise: bool,
    ) -> Option<OwnedObject> {
        None
    }

    pub(crate) fn post_event(_app: ObjectRef, _event: ObjectRef, _at_start: bool) -> bool {
        false
    }

    pub(crate) fn new_buffer_with_length(_device: ObjectRef, _length: u64) -> Option<ObjectRef> {
        None
    }

    pub(crate) fn buffer_contents(_buffer: ObjectRef) -> *mut u8 {
        core::ptr::null_mut()
    }

    pub(crate) fn buffer_length(_buffer: ObjectRef) -> u64 {
        0
    }

    pub(crate) fn new_command_queue(_device: ObjectRef) -> Option<ObjectRef> {
        None
    }

    pub(crate) fn command_buffer(_queue: ObjectRef) -> Option<ObjectRef> {
        None
    }

    pub(crate) fn blit_encoder(_cmd_buf: ObjectRef) -> Option<ObjectRef> {
        None
    }

    pub(crate) fn copy_bytes(
        _encoder: ObjectRef,
        _src: ObjectRef,
        _src_off: u64,
        _dst: ObjectRef,
        _dst_off: u64,
        _size: u64,
    ) {
    }

    pub(crate) fn end_encoding(_encoder: ObjectRef) {}

    pub(crate) fn commit(_cmd_buf: ObjectRef) {}

    pub(crate) fn wait_until_completed(_cmd_buf: ObjectRef) {}

    pub(crate) fn command_buffer_status(_cmd_buf: ObjectRef) -> u64 {
        4
    }

    pub(crate) fn attach_layer_to_window(_window: ObjectRef, _layer: ObjectRef) -> bool {
        false
    }

    pub(crate) fn detach_layer_from_window(_window: ObjectRef) -> bool {
        false
    }

    pub(crate) fn configure_metal_layer(
        _layer: ObjectRef,
        _device: ObjectRef,
        _pixel_format: u64,
        _width: f64,
        _height: f64,
        _contents_scale: f64,
    ) {
    }

    pub(crate) fn set_metal_layer_drawable_size(_layer: ObjectRef, _width: f64, _height: f64) {}

    pub(crate) fn next_drawable(_layer: ObjectRef) -> Option<ObjectRef> {
        None
    }

    pub(crate) fn present_drawable(_drawable: ObjectRef) {}

    pub(crate) fn display_link_available() -> bool {
        false
    }

    pub(crate) fn create_metal_display_link(_layer: ObjectRef) -> Option<OwnedObject> {
        None
    }

    pub(crate) fn display_link_add_to_current_run_loop(_link: ObjectRef) -> bool {
        false
    }

    pub(crate) fn display_link_invalidate(_link: ObjectRef) {}

    pub(crate) fn display_link_set_paused(_link: ObjectRef, _paused: bool) {}
}

/// An owned native object handle with balanced release on drop.
///
/// Construction is only possible through the ABI module, which adopts or
/// retains exactly when the platform provides the object.
#[derive(Debug)]
pub(crate) struct OwnedObject(ObjectRef);

impl OwnedObject {
    pub(crate) const fn from_ref(reference: ObjectRef) -> Self {
        Self(reference)
    }

    pub(crate) const fn as_ref(&self) -> ObjectRef {
        self.0
    }
}

impl Drop for OwnedObject {
    fn drop(&mut self) {
        apple::release_object(self.0);
    }
}

#[allow(unused_imports)]
pub(crate) use apple::{
    app_run, attach_layer_to_window, blit_encoder, buffer_contents, center_window,
    color_attachment_at, color_attachments, command_buffer, command_buffer_status, commit,
    configure_metal_layer, copy_bytes, create_metal_display_link, create_window,
    default_metal_device, detach_layer_from_window, display_link_add_to_current_run_loop,
    display_link_available, display_link_invalidate, display_link_set_paused, drawable_texture,
    end_encoding, event_characters_utf8, event_has_precise_deltas, event_is_repeat, event_key_code,
    event_modifier_flags, event_momentum_phase, event_raw_type, event_scroll_deltas,
    finish_launching, flush_core_animation, install_source_text, is_main_thread,
    make_key_and_order_front, new_buffer_with_length, new_command_queue, new_metal_layer,
    next_drawable, next_event_blocking, next_event_polled, post_event, present_drawable,
    present_drawable_on_buffer, release_object, render_command_encoder, render_pass_descriptor,
    retain_object, send_event, set_activation_policy_accessory, set_activation_policy_regular,
    set_attachment_clear_color, set_attachment_load_clear, set_attachment_store,
    set_attachment_texture, set_content_size, set_metal_layer_drawable_size, shared_application,
    shared_application_owned, synthesize_key_event, synthesize_scroll_event, update_windows,
    wait_until_completed, window_backing_scale,
};
