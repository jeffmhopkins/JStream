//! Drawing preview frames into the canvas's `<canvas>` element: H.264 through
//! WebCodecs' `VideoDecoder`, JPEG through `createImageBitmap`.
//!
//! WebCodecs is still behind `web_sys_unstable_apis` in `web-sys`, so the browser
//! objects are reached through `js_sys::Reflect` instead of typed bindings: no build
//! flag and no new web-sys features. Native builds (tests, clippy) get a stand-in
//! that draws nothing.
//!
//! Docs: crates/jstream-ui/README.md#canvas

#[cfg(target_arch = "wasm32")]
pub use web::Screen;

#[cfg(not(target_arch = "wasm32"))]
pub use native::Screen;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use jstream_control::protocol::BinaryHeader;

    /// The native stand-in: there is no browser to draw in.
    pub struct Screen;

    impl Screen {
        /// Whether the browser can decode H.264. Never, natively.
        pub fn supports_h264() -> bool {
            false
        }

        /// Draws into `canvas`; natively there is nothing to draw on.
        pub fn new<T>(_canvas: T, _on_first: impl Fn() + 'static) -> Option<Screen> {
            None
        }

        /// Shows one frame.
        ///
        /// # Errors
        /// Never, natively.
        pub fn show(&mut self, _header: &BinaryHeader, _payload: &[u8]) -> Result<(), String> {
            Ok(())
        }

        /// Whether the H.264 decoder has failed, so the stream should fall back to JPEG.
        pub fn h264_failed(&self) -> bool {
            false
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use crate::canvas::preview::{H264Feed, Step};
    use jstream_control::protocol::{BinaryHeader, PreviewCodec};
    use leptos::web_sys::js_sys::{Array, Function, Object, Reflect, Uint8Array};
    use leptos::web_sys::wasm_bindgen::closure::Closure;
    use leptos::web_sys::wasm_bindgen::{JsCast, JsValue};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    fn global(name: &str) -> Option<JsValue> {
        Reflect::get(&leptos::web_sys::js_sys::global(), &JsValue::from_str(name))
            .ok()
            .filter(|value| !value.is_undefined() && !value.is_null())
    }

    fn get(target: &JsValue, key: &str) -> Result<JsValue, String> {
        Reflect::get(target, &JsValue::from_str(key)).map_err(describe)
    }

    fn set(target: &JsValue, key: &str, value: &JsValue) -> Result<(), String> {
        Reflect::set(target, &JsValue::from_str(key), value)
            .map(|_| ())
            .map_err(describe)
    }

    fn call(target: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, String> {
        let function: Function = get(target, method)?
            .dyn_into()
            .map_err(|_| format!("{method} is not a function"))?;
        function
            .apply(target, &args.iter().collect::<Array>())
            .map_err(describe)
    }

    fn construct(class: &str, args: &[JsValue]) -> Result<JsValue, String> {
        let constructor: Function = global(class)
            .ok_or_else(|| format!("this browser has no {class}"))?
            .dyn_into()
            .map_err(|_| format!("{class} is not a constructor"))?;
        Reflect::construct(&constructor, &args.iter().collect::<Array>()).map_err(describe)
    }

    fn object(fields: &[(&str, JsValue)]) -> Result<JsValue, String> {
        let object = JsValue::from(Object::new());
        for (key, value) in fields {
            set(&object, key, value)?;
        }
        Ok(object)
    }

    /// A JavaScript exception as text, for the canvas's message.
    fn describe(error: JsValue) -> String {
        error
            .as_string()
            .or_else(|| {
                Reflect::get(&error, &JsValue::from_str("message"))
                    .ok()?
                    .as_string()
            })
            .unwrap_or_else(|| "the browser refused".to_owned())
    }

    /// Draws decoded pictures into the canvas's 2D context.
    struct Painter {
        context: JsValue,
        /// A picture has been drawn.
        drew: Cell<bool>,
        /// Called once, when the first picture is drawn.
        on_first: Box<dyn Fn()>,
        /// The screen is gone: pictures still arriving are closed, not drawn.
        closed: Cell<bool>,
    }

    impl Painter {
        /// Draws `image` (a `VideoFrame` or an `ImageBitmap`), then closes it.
        fn draw(&self, image: JsValue) {
            if !self.closed.get() {
                let zero = JsValue::from_f64(0.0);
                if call(
                    &self.context,
                    "drawImage",
                    &[image.clone(), zero.clone(), zero],
                )
                .is_ok()
                    && !self.drew.replace(true)
                {
                    (self.on_first)();
                }
            }
            let _closed = call(&image, "close", &[]);
        }
    }

    /// JPEG decoding through `createImageBitmap`, one picture at a time. While one is
    /// decoding, only the newest picture waits, so a slow decode shows the latest
    /// picture instead of a backlog.
    struct Jpegs {
        painter: Rc<Painter>,
        busy: Cell<bool>,
        pending: RefCell<Option<Vec<u8>>>,
    }

    /// Decodes and draws one JPEG, or keeps it as the newest waiting one.
    fn decode_jpeg(jpegs: &Rc<Jpegs>, payload: &[u8]) -> Result<(), String> {
        if jpegs.busy.get() {
            *jpegs.pending.borrow_mut() = Some(payload.to_vec());
            return Ok(());
        }
        let parts = Array::of1(&Uint8Array::from(payload));
        let blob = construct(
            "Blob",
            &[
                JsValue::from(parts),
                object(&[("type", JsValue::from_str("image/jpeg"))])?,
            ],
        )?;
        let promise = call(
            &leptos::web_sys::js_sys::global(),
            "createImageBitmap",
            &[blob],
        )?;
        // One-shot closures free themselves once called, and hold the decoder state
        // alive until then, so a decode still running when the screen goes is harmless.
        let done = {
            let jpegs = Rc::clone(jpegs);
            Closure::once_into_js(move |bitmap: JsValue| {
                jpegs.painter.draw(bitmap);
                next_jpeg(&jpegs);
            })
        };
        let failed = {
            let jpegs = Rc::clone(jpegs);
            Closure::once_into_js(move |_: JsValue| next_jpeg(&jpegs))
        };
        call(&promise, "then", &[done, failed])?;
        jpegs.busy.set(true);
        Ok(())
    }

    /// A decode finished: start on the newest waiting picture, if any.
    fn next_jpeg(jpegs: &Rc<Jpegs>) {
        jpegs.busy.set(false);
        let waiting = jpegs.pending.borrow_mut().take();
        if let Some(payload) = waiting
            && !jpegs.painter.closed.get()
        {
            // A picture the browser refuses is skipped; the next one may decode.
            let _shown = decode_jpeg(jpegs, &payload);
        }
    }

    /// The `<canvas>` the preview is drawn in, and the decoders that feed it.
    pub struct Screen {
        canvas: JsValue,
        painter: Rc<Painter>,
        /// The `VideoDecoder`, once an H.264 keyframe configured it.
        decoder: Option<JsValue>,
        feed: H264Feed,
        /// Set by the decoder's error callback.
        failed: Rc<Cell<bool>>,
        /// Draws each decoded `VideoFrame`, then closes it.
        on_frame: Closure<dyn FnMut(JsValue)>,
        on_error: Closure<dyn FnMut(JsValue)>,
        jpegs: Rc<Jpegs>,
    }

    impl Screen {
        /// Whether this browser has WebCodecs' H.264 decoding classes.
        pub fn supports_h264() -> bool {
            global("VideoDecoder").is_some() && global("EncodedVideoChunk").is_some()
        }

        /// A screen on `canvas`, or `None` if it has no 2D context. `on_first` is
        /// called when the first picture has been drawn.
        pub fn new(
            canvas: leptos::web_sys::HtmlCanvasElement,
            on_first: impl Fn() + 'static,
        ) -> Option<Screen> {
            let canvas = JsValue::from(canvas);
            let context = call(&canvas, "getContext", &[JsValue::from_str("2d")]).ok()?;
            if context.is_null() {
                return None;
            }
            let painter = Rc::new(Painter {
                context,
                drew: Cell::new(false),
                on_first: Box::new(on_first),
                closed: Cell::new(false),
            });
            let failed = Rc::new(Cell::new(false));
            let on_frame = {
                let painter = Rc::clone(&painter);
                Closure::<dyn FnMut(JsValue)>::new(move |frame| painter.draw(frame))
            };
            let on_error = {
                let failed = Rc::clone(&failed);
                Closure::<dyn FnMut(JsValue)>::new(move |_| failed.set(true))
            };
            let jpegs = Rc::new(Jpegs {
                painter: Rc::clone(&painter),
                busy: Cell::new(false),
                pending: RefCell::new(None),
            });
            Some(Screen {
                canvas,
                painter,
                decoder: None,
                feed: H264Feed::default(),
                failed,
                on_frame,
                on_error,
                jpegs,
            })
        }

        /// Whether the H.264 decoder has failed, so the stream should fall back to JPEG.
        pub fn h264_failed(&self) -> bool {
            self.failed.get()
        }

        /// Shows one frame: sizes the canvas to the picture, then decodes and draws it.
        ///
        /// # Errors
        /// What the browser refused, as text.
        pub fn show(&mut self, header: &BinaryHeader, payload: &[u8]) -> Result<(), String> {
            let (width, height) = (u32::from(header.width), u32::from(header.height));
            if width == 0 || height == 0 {
                return Ok(());
            }
            for (key, value) in [("width", width), ("height", height)] {
                if get(&self.canvas, key)?.as_f64() != Some(f64::from(value)) {
                    set(&self.canvas, key, &JsValue::from(value))?;
                }
            }
            match header.codec {
                Some(PreviewCodec::H264) => self.h264(header, payload),
                Some(PreviewCodec::Jpeg) => self.jpeg(payload),
                _ => Ok(()),
            }
        }

        fn h264(&mut self, header: &BinaryHeader, payload: &[u8]) -> Result<(), String> {
            if self.failed.get() {
                return Ok(());
            }
            let backlog = match &self.decoder {
                Some(decoder) => get(decoder, "decodeQueueSize")?.as_f64().unwrap_or(0.0) as u32,
                None => 0,
            };
            let key = match self.feed.step(header.keyframe, payload, backlog) {
                Step::Skip => return Ok(()),
                Step::Decode { key } => key,
                Step::Configure(codec) => {
                    self.configure(&codec)?;
                    true
                }
            };
            let Some(decoder) = &self.decoder else {
                return Ok(());
            };
            let data = JsValue::from(Uint8Array::from(payload));
            let chunk = construct(
                "EncodedVideoChunk",
                &[object(&[
                    ("type", JsValue::from_str(if key { "key" } else { "delta" })),
                    ("timestamp", JsValue::from_f64(header.pts_us as f64)),
                    ("data", data),
                ])?],
            )?;
            call(decoder, "decode", &[chunk]).map(|_| ())
        }

        /// Opens a decoder for `codec`, closing the one before.
        fn configure(&mut self, codec: &str) -> Result<(), String> {
            if let Some(old) = self.decoder.take() {
                let _closed = call(&old, "close", &[]);
            }
            let decoder = construct(
                "VideoDecoder",
                &[object(&[
                    ("output", JsValue::from(self.on_frame.as_ref())),
                    ("error", JsValue::from(self.on_error.as_ref())),
                ])?],
            )?;
            let config = object(&[
                ("codec", JsValue::from_str(codec)),
                ("optimizeForLatency", JsValue::TRUE),
            ])?;
            if let Err(error) = call(&decoder, "configure", &[config]) {
                self.failed.set(true);
                self.feed.reset();
                return Err(error);
            }
            self.decoder = Some(decoder);
            Ok(())
        }

        fn jpeg(&mut self, payload: &[u8]) -> Result<(), String> {
            decode_jpeg(&self.jpegs, payload)
        }
    }

    impl Drop for Screen {
        fn drop(&mut self) {
            self.painter.closed.set(true);
            if let Some(decoder) = self.decoder.take() {
                let _closed = call(&decoder, "close", &[]);
            }
        }
    }
}
