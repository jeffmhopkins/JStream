//! The CEF callbacks: app (command-line switches), client, render, audio, display
//! (console), life span, and the tasks that start and end the measured window. Each one
//! forwards to [`Shared`], which owns the state.

use std::os::raw::c_int;
use std::sync::Arc;

use cef::*;

use super::Shared;

/// The handle every callback object carries.
#[derive(Clone)]
pub struct Ctx(pub Arc<Shared>);

wrap_app! {
    pub struct S4App;

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else {
                return;
            };
            // The test page plays its tones without a click.
            command_line.append_switch_with_value(
                Some(&"autoplay-policy".into()),
                Some(&"no-user-gesture-required".into()),
            );
            // No first-run, crash-restore or keychain prompts in a headless browser.
            command_line.append_switch(Some(&"no-first-run".into()));
            command_line.append_switch(Some(&"noerrdialogs".into()));
        }
    }
}

wrap_client! {
    pub struct S4Client {
        render: RenderHandler,
        audio: AudioHandler,
        display: DisplayHandler,
        life_span: LifeSpanHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn audio_handler(&self) -> Option<AudioHandler> {
            Some(self.audio.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }
    }
}

pub fn client(ctx: &Ctx) -> Client {
    S4Client::new(
        S4Render::new(ctx.clone()),
        S4Audio::new(ctx.clone()),
        S4Display::new(ctx.clone()),
        S4LifeSpan::new(ctx.clone()),
    )
}

wrap_render_handler! {
    pub struct S4Render {
        ctx: Ctx,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let (width, height) = self.ctx.0.view_size();
                rect.x = 0;
                rect.y = 0;
                rect.width = width as c_int;
                rect.height = height as c_int;
            }
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> c_int {
            // 1 CSS pixel = 1 texture pixel, whatever the desktop's scaling.
            let Some(screen_info) = screen_info else {
                return 0;
            };
            screen_info.device_scale_factor = 1.0;
            1
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            _buffer: *const u8,
            _width: c_int,
            _height: c_int,
        ) {
            self.ctx.0.on_software_paint(type_ == PaintElementType::VIEW);
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            if type_ != PaintElementType::VIEW {
                self.ctx.0.on_popup_paint();
                return;
            }
            self.ctx.0.on_accelerated_paint(info);
        }
    }
}

wrap_audio_handler! {
    pub struct S4Audio {
        ctx: Ctx,
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
            let (rate, frames) = super::REQUESTED_AUDIO;
            params.channel_layout = ChannelLayout::LAYOUT_STEREO;
            params.sample_rate = rate;
            params.frames_per_buffer = frames;
            // Capture this browser's audio.
            1
        }

        fn on_audio_stream_started(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&AudioParameters>,
            channels: c_int,
        ) {
            let params = params.map(|p| {
                (p.sample_rate, p.frames_per_buffer, *p.channel_layout.as_ref() as i32)
            });
            self.ctx.0.on_audio_started(params, channels);
        }

        fn on_audio_stream_packet(
            &self,
            _browser: Option<&mut Browser>,
            data: *mut *const f32,
            frames: c_int,
            pts: i64,
        ) {
            self.ctx.0.on_audio_packet(data, frames, pts);
        }

        fn on_audio_stream_stopped(&self, _browser: Option<&mut Browser>) {
            self.ctx.0.on_audio_stopped();
        }

        fn on_audio_stream_error(&self, _browser: Option<&mut Browser>, message: Option<&CefString>) {
            self.ctx.0.on_audio_error(message.map(|m| m.to_string()).unwrap_or_default());
        }
    }
}

wrap_display_handler! {
    pub struct S4Display {
        ctx: Ctx,
    }

    impl DisplayHandler {
        fn on_console_message(
            &self,
            _browser: Option<&mut Browser>,
            _level: LogSeverity,
            message: Option<&CefString>,
            _source: Option<&CefString>,
            _line: c_int,
        ) -> c_int {
            if let Some(message) = message {
                self.ctx.0.on_console(message.to_string());
            }
            // Let CEF log it too.
            0
        }
    }
}

wrap_life_span_handler! {
    pub struct S4LifeSpan {
        ctx: Ctx,
    }

    impl LifeSpanHandler {
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.ctx.0.on_browser_closed();
            quit_message_loop();
        }
    }
}

wrap_task! {
    pub struct StartWindow {
        ctx: Ctx,
    }

    impl Task {
        fn execute(&self) {
            self.ctx.0.start_window();
        }
    }
}

wrap_task! {
    pub struct EndWindow {
        ctx: Ctx,
        browser: Browser,
    }

    impl Task {
        fn execute(&self) {
            self.ctx.0.end_window();
            match self.browser.host() {
                // Closing ends in `on_before_close`, which quits the message loop.
                Some(host) => host.close_browser(1),
                None => quit_message_loop(),
            }
        }
    }
}

wrap_task! {
    pub struct QuitLoop;

    impl Task {
        fn execute(&self) {
            quit_message_loop();
        }
    }
}
