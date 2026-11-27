use crate::app::StreamingSession;
use crate::shell::egui_painter::SdlEguiPainter;
use crate::streaming::video::{DirectVideoOutput, VideoTextureTarget};
use anyhow::{Context, Result};
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::{Canvas, Texture};
use sdl2::video::Window;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

pub const WIDTH: u32 = 960;
pub const HEIGHT: u32 = 544;

pub struct VitaSurface {
    pub(crate) canvas: Canvas<Window>,
    video_textures: Option<Vec<Texture>>,
    displayed_video_texture: Option<usize>,
    direct_video_output: Option<Arc<DirectVideoOutput>>,
    video_width: u32,
    video_height: u32,
    pending_display_timestamps: Option<(Instant, Instant, Instant, crate::streaming::hud::DecodeStages)>, 
    egui_painter: SdlEguiPainter,
    last_video_present:Option<Instant>,
    benchmark:crate::streaming::benchmark::Benchmark,
    benchmark_internal:bool,
    direct_frame:bool,
}

impl VitaSurface {
    pub fn new(video: &sdl2::VideoSubsystem) -> Result<Self> {
        let window = video
            .window("GreenVita", WIDTH, HEIGHT)
            .position_centered()
            .build()
            .context("failed to create SDL Vita window")?;
        let mut canvas = window
            .into_canvas()
            .accelerated()
            .build()
            .map_err(anyhow::Error::msg)
            .context("failed to create SDL Vita renderer")?;
        canvas
            .set_logical_size(WIDTH, HEIGHT)
            .map_err(anyhow::Error::msg)
            .context("failed to set Vita logical render size")?;

        Ok(Self {
            canvas,
            video_textures: None,
            displayed_video_texture: None,
            direct_video_output: None,
            video_width: 0,
            video_height: 0,
            pending_display_timestamps: None,
            egui_painter: SdlEguiPainter::default(),
            last_video_present:None,
            benchmark:crate::streaming::benchmark::Benchmark::default(),
            benchmark_internal:false,
            direct_frame:false,
        })
    }

    /// Where the video quad lands on screen - accounts for letterboxing, see `fit_rect`.
    pub fn video_rect(&self) -> sdl2::rect::Rect {
        Self::fit_rect(self.video_width, self.video_height, WIDTH, HEIGHT)
    }

    pub fn prepare_video_output(&mut self,streaming:Option<&StreamingSession>)->Result<()> {if let Some(s)=streaming{self.ensure_direct_video_output(s)}else{self.detach_direct_video_output();Ok(())}}
    pub fn sync_video_frame(&mut self, streaming: Option<&StreamingSession>) -> Result<bool> {
        let Some(streaming) = streaming else {
            self.detach_direct_video_output();
            return Ok(false);
        };
        self.ensure_direct_video_output(streaming)?;

        let Some(frame)=self.direct_video_output.as_ref().and_then(|output|output.claim_latest()) else{return Ok(false);};
        self.direct_frame=false;
        self.accept_video_frame(frame)
    }

    pub fn try_sync_ready_video_frame(&mut self)->Result<bool>{
        let Some(frame)=self.direct_video_output.as_ref().and_then(|o|o.try_claim_latest()) else{return Ok(false);};
        self.direct_frame=true;
        self.accept_video_frame(frame)
    }
    fn accept_video_frame(&mut self,frame:crate::streaming::video::DecodedFrame)->Result<bool>{
        let index=frame.texture_index;
        if index>=self.video_textures.as_ref().map_or(0,Vec::len){anyhow::bail!("decoder returned invalid direct texture index {index}");}
        self.displayed_video_texture=Some(index);
        self.pending_display_timestamps=Some((frame.decoded_at,frame.enqueued_at,Instant::now(),frame.stages));
        Ok(true)
    }

    pub fn benchmark_tick(&mut self,active:bool,mode:u8,profile:u8){
        let internal=crate::experimental::active_internal();if internal!=self.benchmark_internal{self.benchmark.reset();self.benchmark_internal=internal;}
        if let Some(report)=self.benchmark.tick(Instant::now(),active,mode,profile){crate::diagnostics::benchmark_result(&report);}
    }
    pub fn reset_benchmark(&mut self){self.benchmark.reset();}

    fn ensure_direct_video_output(&mut self, streaming: &StreamingSession) -> Result<()> {
        let output = streaming.direct_video_output();
        let output_is_current = self
            .direct_video_output
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &output));
        if !output_is_current && self.direct_video_output.is_some() {
            self.detach_direct_video_output();
        }
        if !output.decoder_ready.load(Ordering::Acquire) {
            return Ok(());
        }
        if output_is_current && self.video_textures.is_some() {
            return Ok(());
        }

        self.detach_direct_video_output();
        let (width, height) = (output.width, output.height);
        // BGR565 is the only supported hardware output. Keep the two/three
        // target experiment independent of the removed YUV color path.
        let count=if streaming_lab_triple(){3}else{2};
        let mut textures=Vec::with_capacity(count);
        let mut targets=Vec::with_capacity(count);
        for _ in 0..count{
            let Ok(mut texture)=self.canvas.create_texture_streaming(PixelFormatEnum::BGR565,width,height)else{break;};
            let target=texture.with_lock(None,|pixels,pitch|{
                let valid=pitch>=width as usize*2 && pitch%2==0 && pixels.len()>=pitch*height as usize;
                valid.then_some(VideoTextureTarget{ptr:pixels.as_mut_ptr() as usize,pitch:pitch as u32,capacity:pixels.len().min(u32::MAX as usize) as u32})
            });
            match target{
                Ok(Some(t))=>{textures.push(texture);targets.push(t);}
                _=>{unsafe{texture.destroy();}break;}
            }
        }
        if textures.len()<2{
            // These targets were never registered or submitted to the GPU.
            for texture in textures{unsafe{texture.destroy();}}
            anyhow::bail!("could not allocate two compatible BGR565 video textures");
        }
        crate::experimental::record_texture(textures.len());
        crate::diagnostics::step("lab_texture",if textures.len()==3{"TRIPLE"}else{"DOUBLE"});
        output.set_targets(targets);
        self.video_textures = Some(textures);
        self.displayed_video_texture = None;
        self.direct_video_output = Some(output);
        self.video_width = width;
        self.video_height = height;
        Ok(())
    }

    fn detach_direct_video_output(&mut self) {
        if let Some(output) = self.direct_video_output.take() {
            output.clear_targets();
        }
        self.video_textures = None;
        self.displayed_video_texture = None;
        self.video_width = 0;
        self.video_height = 0;
        self.pending_display_timestamps = None;
        self.last_video_present=None;
        self.benchmark.reset();
        self.direct_frame=false;
    }

    pub fn draw_scene(&mut self, show_video: bool) -> Result<()> {
        self.canvas.set_draw_color(sdl2::pixels::Color::BLACK);
        self.canvas.clear();

        if show_video
            && let Some(index) = self.displayed_video_texture
            && let Some(texture) = self
                .video_textures
                .as_ref()
                .map(|textures| &textures[index])
        {
            let destination = self.video_rect();
            self.canvas
                .copy(texture, None, destination)
                .map_err(anyhow::Error::msg)
                .context("failed to draw SDL BGR565 video frame")?;
        }

        Ok(())
    }

    pub fn prepare_egui(&mut self,pixels_per_point:f32,primitives:&[egui::ClippedPrimitive],textures_delta:&egui::TexturesDelta)->Result<()> {
        self.egui_painter.prepare(&mut self.canvas,[WIDTH,HEIGHT],pixels_per_point,primitives,textures_delta)
    }

    pub fn paint_egui(&mut self,textures_delta:&egui::TexturesDelta) -> Result<()> {
        self.egui_painter.draw_prepared(&mut self.canvas,textures_delta)?;
        let before_present=Instant::now();
        self.canvas.present();
        if let Some((decoded_at, enqueued_at,claimed_at,stages)) = self.pending_display_timestamps.take() {
            let now = Instant::now();
            let gap_us=self.last_video_present.replace(now).map(|at|now.saturating_duration_since(at).as_micros() as u64).unwrap_or(0);
            self.benchmark.observe_verified(now,stages.verified.then(||now.saturating_duration_since(enqueued_at).as_micros() as u64),gap_us,claimed_at.saturating_duration_since(decoded_at).as_micros() as u64,self.direct_frame,stages.decoder_age_us);
            self.direct_frame=false;
            crate::streaming::video::metrics::METRICS.presented.fetch_add(1,Ordering::Relaxed);
            crate::streaming::video::metrics::presented(crate::streaming::hud::PresentedStages{decode:stages,present_gap_us:gap_us,display_us:now.saturating_duration_since(decoded_at).as_micros() as u64,pre_present_us:before_present.saturating_duration_since(decoded_at).as_micros() as u64,claim_us:claimed_at.saturating_duration_since(decoded_at).as_micros() as u64,draw_us:before_present.saturating_duration_since(claimed_at).as_micros() as u64,present_call_us:now.saturating_duration_since(before_present).as_micros() as u64,total_us:now.saturating_duration_since(enqueued_at).as_micros() as u64});
            crate::streaming::video::metrics::METRICS.display_queue_us.store(
                now.saturating_duration_since(decoded_at).as_micros() as u64,
                Ordering::Relaxed,
            );
            crate::streaming::video::metrics::METRICS.total_video_pipeline_us.store(
                if stages.verified{now.saturating_duration_since(enqueued_at).as_micros() as u64}else{u64::MAX},
                Ordering::Relaxed,
            );
        }
        Ok(())
    }

    fn fit_rect(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> sdl2::rect::Rect {
        if src_w == 0 || src_h == 0 {
            return sdl2::rect::Rect::new(0, 0, dst_w, dst_h);
        }
        let src_aspect = src_w as f32 / src_h as f32;
        let dst_aspect = dst_w as f32 / dst_h as f32;
        if src_aspect > dst_aspect {
            let height = (dst_w as f32 / src_aspect).round() as u32;
            let y = ((dst_h - height) / 2) as i32;
            sdl2::rect::Rect::new(0, y, dst_w, height)
        } else {
            let width = (dst_h as f32 * src_aspect).round() as u32;
            let x = ((dst_w - width) / 2) as i32;
            sdl2::rect::Rect::new(x, 0, width, dst_h)
        }
    }
}

fn streaming_lab_triple()->bool{crate::experimental::triple_enabled()}
