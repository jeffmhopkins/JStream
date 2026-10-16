//! CEF's callbacks: the app (command-line switches), and per page a client with its
//! render, audio, life-span, load and request handlers, each forwarding to the page's
//! [`PageShared`]. Plus a task that runs a closure on CEF's UI thread.

use std::os::raw::c_int;
use std::sync::Arc;

use cef::*;

use super::PageShared;

/// The handle every one of a page's callback objects carries.
#[derive(Clone)]
pub(crate) struct Page(pub(crate) Arc<PageShared>);

wrap_app! {
    pub(crate) struct JstreamApp;

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else {
                return;
            };
            // Alerts and overlays play sound with nobody there to click.
            command_line.append_switch_with_value(
                Some(&"autoplay-policy".into()),
                Some(&"no-user-gesture-required".into()),
            );
            // No first-run, crash-restore or error dialogs in a browser nobody sees.
            command_line.append_switch(Some(&"no-first-run".into()));
            command_line.append_switch(Some(&"noerrdialogs".into()));
        }
    }
}

/// The browser process's app.
pub(crate) fn app() -> App {
    JstreamApp::new()
}

wrap_task! {
    pub(crate) struct FnTask {
        f: Arc<dyn Fn() + Send + Sync>,
    }

    impl Task {
        fn execute(&self) {
            (self.f)();
        }
    }
}

/// A task that runs `f` on the thread it is posted to.
pub(crate) fn task(f: impl Fn() + Send + Sync + 'static) -> Task {
    FnTask::new(Arc::new(f))
}

wrap_client! {
    pub(crate) struct PageClient {
        render: RenderHandler,
        audio: AudioHandler,
        life_span: LifeSpanHandler,
        load: LoadHandler,
        request: RequestHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn audio_handler(&self) -> Option<AudioHandler> {
            Some(self.audio.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.request.clone())
        }
    }
}

/// The client for one page's browser.
pub(crate) fn client(page: &Arc<PageShared>) -> Client {
    let page = Page(Arc::clone(page));
    PageClient::new(
        PageRender::new(page.clone()),
        PageAudioHandler::new(page.clone()),
        PageLifeSpan::new(page.clone()),
        PageLoad::new(page.clone()),
        PageRequests::new(PageResources::new(page)),
    )
}

wrap_render_handler! {
    pub(crate) struct PageRender {
        page: Page,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let (width, height) = self.page.0.size;
                rect.x = 0;
                rect.y = 0;
                rect.width = c_int::try_from(width).unwrap_or(c_int::MAX);
                rect.height = c_int::try_from(height).unwrap_or(c_int::MAX);
            }
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> c_int {
            // One CSS pixel is one texture pixel, whatever the desktop's scaling.
            let Some(screen_info) = screen_info else {
                return 0;
            };
            screen_info.device_scale_factor = 1.0;
            1
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            // Popups (`<select>` lists) paint separately; an overlay has no use for them.
            if type_ != PaintElementType::VIEW {
                return;
            }
            let Some(info) = info else {
                return;
            };
            self.page.0.paint(info.shared_texture_handle);
        }
    }
}

wrap_audio_handler! {
    pub(crate) struct PageAudioHandler {
        page: Page,
    }

    impl AudioHandler {
        fn audio_parameters(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&mut AudioParameters>,
        ) -> c_int {
            let Some(params) = params else {
                return 0;
            };
            params.channel_layout = ChannelLayout::LAYOUT_STEREO;
            params.sample_rate = c_int::try_from(crate::PageAudio::rate()).unwrap_or(48_000);
            // 10 ms packets, which spike S4 saw CEF honour.
            params.frames_per_buffer = params.sample_rate / 100;
            // Capture this browser's audio instead of playing it.
            1
        }

        fn on_audio_stream_started(
            &self,
            _browser: Option<&mut Browser>,
            _params: Option<&AudioParameters>,
            channels: c_int,
        ) {
            let channels = usize::try_from(channels).unwrap_or(0);
            self.page.0.channels.store(channels, std::sync::atomic::Ordering::Release);
        }

        fn on_audio_stream_packet(
            &self,
            _browser: Option<&mut Browser>,
            data: *mut *const f32,
            frames: c_int,
            _pts: i64,
        ) {
            // `pts` looks like milliseconds since boot, not CEF's documented Unix time
            // (ARCH › Browser source); the engine stamps packets as they arrive instead.
            let page = &self.page.0;
            let channels = page.channels.load(std::sync::atomic::Ordering::Acquire).min(2);
            let Ok(frames) = usize::try_from(frames) else {
                return;
            };
            if data.is_null() || channels == 0 {
                return;
            }
            let mut planes: [&[f32]; 2] = [&[], &[]];
            for (ch, plane) in planes.iter_mut().enumerate().take(channels) {
                // SAFETY: CEF passes one plane pointer per channel of the stream
                // (`OnAudioStreamStarted`'s count, of which this reads at most two) in
                // `data`, valid for the duration of this callback.
                let samples = unsafe { *data.add(ch) };
                if samples.is_null() {
                    return;
                }
                // SAFETY: each plane holds `frames` `f32` samples, valid and unchanged
                // for the duration of this callback, which the slice doesn't outlive.
                *plane = unsafe { std::slice::from_raw_parts(samples, frames) };
            }
            page.audio(&planes[..channels], frames);
        }

        fn on_audio_stream_error(
            &self,
            _browser: Option<&mut Browser>,
            message: Option<&CefString>,
        ) {
            // CEF reports "Socket closed unexpectedly" whenever a page closes (spike S4):
            // that is the stream's end, not a fault.
            let page = &self.page.0;
            if page.is_closing() {
                return;
            }
            page.audio_failed(message.map(|m| m.to_string()).unwrap_or_default());
        }
    }
}

wrap_life_span_handler! {
    pub(crate) struct PageLifeSpan {
        page: Page,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: c_int,
            _target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut c_int>,
        ) -> c_int {
            // No pop-up windows: nobody would see them, and nothing would close them.
            1
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                self.page.0.created(browser.clone());
            }
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.page.0.closed();
        }
    }
}

wrap_load_handler! {
    pub(crate) struct PageLoad {
        page: Page,
    }

    impl LoadHandler {
        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: c_int,
        ) {
            if let Some(frame) = frame
                && frame.is_main() == 1
            {
                self.page.0.loaded(frame);
            }
        }
    }
}

wrap_request_handler! {
    pub(crate) struct PageRequests {
        resources: ResourceRequestHandler,
    }

    impl RequestHandler {
        fn resource_request_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            _is_navigation: c_int,
            _is_download: c_int,
            _request_initiator: Option<&CefString>,
            _disable_default_handling: Option<&mut c_int>,
        ) -> Option<ResourceRequestHandler> {
            // Every request of the page, navigations included, goes past its policy.
            Some(self.resources.clone())
        }
    }
}

wrap_resource_request_handler! {
    pub(crate) struct PageResources {
        page: Page,
    }

    impl ResourceRequestHandler {
        fn on_before_resource_load(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _callback: Option<&mut Callback>,
        ) -> ReturnValue {
            let Some(request) = request else {
                return ReturnValue::CANCEL;
            };
            let url = CefString::from(&request.url()).to_string();
            let page = &self.page.0;
            match page.policy.check(&url) {
                crate::Verdict::Allow => ReturnValue::CONTINUE,
                crate::Verdict::Deny(_) => {
                    // The URL may be a local path, which the engine's logs must never
                    // name: it gets the count.
                    page.denied();
                    ReturnValue::CANCEL
                }
            }
        }
    }
}
