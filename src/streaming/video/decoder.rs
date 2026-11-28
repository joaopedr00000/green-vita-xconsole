//! PS Vita hardware H.264 decoder (`sceVideodec`/`sceAvcdec`).
use super::memory::{CdramBlock, release_reserved_decoder_cdram};
use super::{DecoderConfig, VideoTextureTarget};
use anyhow::{Result, bail};
use std::os::raw::c_void;
use vitasdk_sys::*;

// The idea of reducing the reference frames came from MattKC on his Vanilla project
// Make sure to check it out, good content :)
const AVCDEC_NUM_REF_FRAMES: u32 = super::AVCDEC_NUM_REF_FRAMES;
// AVCDEC and SDL's Vita GXM renderer both support RGB565 natively. At 960x544 this halves the
// decoder-to-texture traffic from roughly 2 MiB to 1 MiB per frame, which matters at 60 FPS.
const OUTPUT_BYTES_PER_PIXEL: u32 = 2;
const OUTPUT_PIXEL_FORMAT: u32 = SCE_AVCDEC_PIXELFORMAT_RGBA565 as u32;

struct AvcdecLibrary {
    module_loaded: bool,
}

impl AvcdecLibrary {
    fn initialize(width: u32, height: u32, refs:u32) -> Result<Self> {
        let module_loaded = unsafe {
            let loaded_before = sceSysmoduleIsLoaded(SCE_SYSMODULE_AVCDEC);
            let ret = sceSysmoduleLoadModule(SCE_SYSMODULE_AVCDEC);
            if ret >= 0 {
                true
            } else if ret as u32 == SCE_SYSMODULE_ERROR_INVALID_VALUE {
                eprintln!(
                    "sceSysmoduleLoadModule(SCE_SYSMODULE_AVCDEC=0x{SCE_SYSMODULE_AVCDEC:x}) returned {ret:#x}; continuing with SceVideodec imports; is_loaded_before={loaded_before:#x}",
                );
                false
            } else {
                bail!(
                    "sceSysmoduleLoadModule(SCE_SYSMODULE_AVCDEC=0x{SCE_SYSMODULE_AVCDEC:x}) failed: {ret:#x}; is_loaded_before={loaded_before:#x}",
                );
            }
        };

        let init_info = SceVideodecQueryInitInfoHwAvcdec {
            size: size_of::<SceVideodecQueryInitInfoHwAvcdec>() as u32,
            horizontal: width,
            vertical: height,
            numOfRefFrames: refs,
            numOfStreams: 1,
        };
        let ret = unsafe { sceVideodecInitLibrary(SCE_VIDEODEC_TYPE_HW_AVCDEC, &init_info) };
        if ret < 0 {
            if module_loaded {
                unsafe {
                    sceSysmoduleUnloadModule(SCE_SYSMODULE_AVCDEC);
                }
            }
            bail!("sceVideodecInitLibrary failed: {ret:#x}");
        }

        Ok(Self { module_loaded })
    }
}

impl Drop for AvcdecLibrary {
    fn drop(&mut self) {
        unsafe {
            sceVideodecTermLibrary(SCE_VIDEODEC_TYPE_HW_AVCDEC);
            if self.module_loaded {
                sceSysmoduleUnloadModule(SCE_SYSMODULE_AVCDEC);
            }
        }
    }
}

struct AvcdecDecoder(SceAvcdecCtrl);

impl Drop for AvcdecDecoder {
    fn drop(&mut self) {
        unsafe {
            sceAvcdecDeleteDecoder(&mut self.0);
        }
    }
}

pub struct PublicDecoder {
    decoder: AvcdecDecoder,
    _frame_memory: CdramBlock,
    _library: AvcdecLibrary,
    width: u32,
    height: u32,
    last_color_info: Option<[u8; 4]>,
}

impl PublicDecoder {
    pub fn new(config:DecoderConfig)->Result<Self>{
        release_reserved_decoder_cdram();
        let mut last=None;
        for (refs,phy) in [(AVCDEC_NUM_REF_FRAMES,true),(AVCDEC_NUM_REF_FRAMES,false),(1,true),(1,false)]{
            match Self::try_new(config,refs,phy){
                Ok(d)=>return Ok(d),
                Err(e)=>{
                    eprintln!("AVCDEC initialization refs={refs} phycont={phy}: {e:#}");
                    crate::diagnostics::decoder_allocation_attempt(refs,phy,false,0);
                    last=Some(e);
                }
            }
        }
        Err(last.expect("four attempts"))
    }
    fn try_new(config: DecoderConfig,refs:u32,phy:bool) -> Result<Self> {
        unsafe {
            let library = AvcdecLibrary::initialize(config.decode_width, config.decode_height,refs)?;

            let query = SceAvcdecQueryDecoderInfo {
                horizontal: config.decode_width,
                vertical: config.decode_height,
                numOfRefFrames: refs,
            };
            let mut decoder_info = SceAvcdecDecoderInfo { frameMemSize: 0 };
            let ret = sceAvcdecQueryDecoderMemSize(
                SCE_VIDEODEC_TYPE_HW_AVCDEC,
                &query,
                &mut decoder_info,
            );
            if ret < 0 {
                bail!("sceAvcdecQueryDecoderMemSize failed: {ret:#x}");
            }
            if decoder_info.frameMemSize==0{bail!("AVCDEC returned zero frame memory");}
            let frame_memory=if phy{CdramBlock::allocate_phycont("xcloud_hw_video_phy",decoder_info.frameMemSize)?}
                else{CdramBlock::allocate("xcloud_hw_video_frame",decoder_info.frameMemSize)?};
            let mut decoder_control = SceAvcdecCtrl {
                handle: 0,
                frameBuf: SceAvcdecBuf {
                    pBuf: frame_memory.ptr.cast(),
                    size: if phy{frame_memory.capacity()}else{decoder_info.frameMemSize},
                },
            };
            let ret =
                sceAvcdecCreateDecoder(SCE_VIDEODEC_TYPE_HW_AVCDEC, &mut decoder_control, &query);
            if ret < 0 {
                bail!("sceAvcdecCreateDecoder failed: {ret:#x}");
            }
            let decoder = AvcdecDecoder(decoder_control);
            crate::diagnostics::decoder_allocation_attempt(refs,phy,true,frame_memory.capacity());
            crate::diagnostics::decoder_config_info(config.decode_width,config.decode_height,config.output_width,config.output_height,refs,phy);

            Ok(Self {
                decoder,
                _frame_memory: frame_memory,
                _library: library,
                width: config.output_width,
                height: config.output_height,
                last_color_info: None,
            })
        }
    }

    /// Decodes one Access Unit. Returns None when the hardware produces no picture yet.
    pub fn decode(
        &mut self,
        access_unit: &mut Vec<u8>,
        direct_target: VideoTextureTarget,
        input_pts:u64,
    ) -> Result<Option<u64>> {
        let input_size=super::timing::pad_access_unit(access_unit).map_err(anyhow::Error::msg)?;
        unsafe {
            let au = SceAvcdecAu {
                pts: SceVideodecTimeStamp {
                    upper: (input_pts >> 32) as u32,
                    lower: input_pts as u32,
                },
                dts: SceVideodecTimeStamp {
                    upper: (input_pts >> 32) as u32,
                    lower: input_pts as u32,
                },
                es: SceAvcdecBuf {
                    pBuf: access_unit.as_mut_ptr() as *mut c_void,
                    size: input_size,
                },
            };

            let output_ptr = direct_target.ptr as *mut u8;
            let output_pitch = direct_target.pitch / OUTPUT_BYTES_PER_PIXEL;
            let output_capacity = direct_target.capacity;
            if direct_target.pitch % OUTPUT_BYTES_PER_PIXEL != 0 || output_pitch.checked_mul(self.height).and_then(|n|n.checked_mul(OUTPUT_BYTES_PER_PIXEL)).is_none_or(|n|n>output_capacity) {
                bail!("invalid BGR565 target pitch or capacity");
            }
            if output_pitch < self.width {
                bail!(
                    "direct video texture pitch {output_pitch} is smaller than {}",
                    self.width
                );
            }

            let mut picture = SceAvcdecPicture {
                size: size_of::<SceAvcdecPicture>() as u32,
                frame: SceAvcdecFrame {
                    pixelType: OUTPUT_PIXEL_FORMAT,
                    framePitch: output_pitch,
                    frameWidth: self.width,
                    frameHeight: self.height,
                    horizontalSize: self.width,
                    verticalSize: self.height,
                    frameCropLeftOffset: 0,
                    frameCropRightOffset: 0,
                    frameCropTopOffset: 0,
                    frameCropBottomOffset: 0,
                    opt: SceAvcdecFrameOption {
                        rgba: SceAvcdecFrameOptionRGBA {
                            alpha: 0xff,
                            cscCoefficient: 0,
                            reserved: [0; 14],
                        },
                    },
                    pPicture: [output_ptr.cast(), std::ptr::null_mut()],
                },
                info: std::mem::zeroed(),
            };
            let mut picture_ptr: *mut SceAvcdecPicture = &mut picture;
            let mut array_picture = SceAvcdecArrayPicture {
                numOfOutput: 0,
                numOfElm: 1,
                pPicture: &mut picture_ptr,
            };

            let ret = sceAvcdecDecode(&self.decoder.0, &au, &mut array_picture);
            if ret < 0 {
                bail!("sceAvcdecDecode failed: {ret:#x}");
            }
            if array_picture.numOfOutput > 1 { bail!("invalid single-output picture count"); }
            if array_picture.numOfOutput == 0 {
                return Ok(None);
            }

            // `framePitch` is in pixels. Validate the complete byte size before trusting the
            // decoder-provided pitch to write into the SDL texture.
            let row_bytes = picture
                .frame
                .framePitch
                .checked_mul(OUTPUT_BYTES_PER_PIXEL)
                .ok_or_else(|| {
                    anyhow::anyhow!("video frame pitch overflow: {}", picture.frame.framePitch)
                })?;
            let output_len = row_bytes.checked_mul(self.height).ok_or_else(|| {
                anyhow::anyhow!("video output size overflow: {row_bytes} * {}", self.height)
            })?;
            if output_len > output_capacity {
                bail!(
                    "sceAvcdecDecode produced pitch requiring {output_len} bytes, but output buffer has {} bytes",
                    output_capacity
                );
            }
            let color = [picture.info.colourPrimaries, picture.info.transferCharacteristics,
                picture.info.matrixCoefficients, picture.info.videoFullRangeFlag];
            if self.last_color_info != Some(color) {
                crate::diagnostics::decoder_color_info(color, 0);
                self.last_color_info = Some(color);
            }
            Ok(Some(((picture.info.pts.upper as u64)<<32)|picture.info.pts.lower as u64))
        }
    }
}

// SAFETY: the CDRAM blocks and decoder handle have no thread affinity in the underlying SCE API -
// this is only ever moved once (into `VideoDecodeWorker`'s thread), never accessed concurrently.
unsafe impl Send for PublicDecoder {}

use std::time::Instant;
use crate::streaming::hud::DecodeStages;
use super::timing::{Pending,PtsClock};
#[derive(Clone,Copy)]
pub(super) struct FrameTiming{pub enqueued:Instant,pub stages:DecodeStages,pub generation:u64}

pub struct HwVideoDecoder {
    backend: PublicDecoder,
    decode_size:(u32,u32),
    calls_by_result:super::timing::CallStats,
    pending: Pending<(FrameTiming,Instant,u64)>,
    clock:PtsClock,period_us:u64,over_budget:u64,last_submit:Option<Instant>,empty_vcl:u64,empty_other:u64,
    seq:u64, outputs:u64, empty:u64, no_output:u32, peak_no_output:u32, peak_pending:usize,
    call_total_us:u64,call_max_us:u64,frame_lag_sum:u64,frame_lag_max:u64,matched:u64,wait_sum_us:u64,
    started:Instant,last_report:Instant,last_generation:Option<u64>,last_lag:Option<u64>,
    ever_output:bool,
}
impl HwVideoDecoder {
    pub fn new(config:DecoderConfig)->Result<Self>{
        let backend=PublicDecoder::new(config)?;
        crate::diagnostics::step("lab_decoder","PUBLIC");
        Ok(Self{backend,decode_size:(config.decode_width,config.decode_height),calls_by_result:Default::default(),pending:Pending::new(),clock:PtsClock::default(),period_us:0,over_budget:0,last_submit:None,empty_vcl:0,empty_other:0,seq:0,outputs:0,empty:0,no_output:0,peak_no_output:0,peak_pending:0,
            call_total_us:0,call_max_us:0,frame_lag_sum:0,frame_lag_max:0,matched:0,wait_sum_us:0,
            started:Instant::now(),last_report:Instant::now(),last_generation:None,last_lag:None,
            ever_output:false})
    }
    pub(super) fn decode_size(&self)->(u32,u32){self.decode_size}
    pub(super) fn generation_changed(&self,generation:u64)->bool{self.last_generation.is_some_and(|g|g!=generation)}
    pub fn decode(&mut self,data:&mut Vec<u8>,target:VideoTextureTarget,mut timing:FrameTiming,rtp_timestamp:u32,period:Option<u64>,nal_mask:u32)->Result<Option<FrameTiming>>{
        self.last_generation=Some(timing.generation);
        self.seq=self.seq.saturating_add(1);
        let pts=self.clock.next(rtp_timestamp,period);
        self.period_us=period.unwrap_or(0);
        let started=Instant::now();
        let gap_us=self.last_submit.replace(started).map_or(0,|v|started.saturating_duration_since(v).as_micros() as u64);
        let bytes=data.len();
        let returned=self.backend.decode(data,target,pts)?;
        let call_us=started.elapsed().as_micros() as u64;
        self.calls_by_result.record(returned.is_some(),call_us);
        timing.stages.decode_us=call_us;
        self.pending.push(pts,(timing,started,self.seq));
        self.call_total_us=self.call_total_us.saturating_add(call_us);
        self.call_max_us=self.call_max_us.max(call_us);
        let result=if let Some(id)=returned{
            self.outputs+=1;self.no_output=0;
            if !self.ever_output{
                self.ever_output=true;
                crate::diagnostics::decoder_startup_info(self.seq,self.pending.len(),self.started.elapsed().as_micros() as u64);
            }
            if let Some((mut original,submitted,ordinal))=self.pending.take(id){
                let lag=self.seq.saturating_sub(ordinal);
                if self.last_lag!=Some(lag){
                    crate::diagnostics::decoder_lag_change(self.last_lag,lag,self.pending.len());
                    self.last_lag=Some(lag);
                }
                self.frame_lag_sum=self.frame_lag_sum.saturating_add(lag);
                self.frame_lag_max=self.frame_lag_max.max(lag);self.matched+=1;
                original.stages.verified=true;
                original.stages.decoder_age_us=submitted.elapsed().as_micros() as u64;
                original.stages.decoder_pending=self.pending.len() as u32;
                if original.stages.decoder_age_us>100_000{self.over_budget+=1;}
                self.wait_sum_us=self.wait_sum_us.saturating_add(original.stages.decoder_age_us.saturating_sub(original.stages.decode_us));
                if original.generation==timing.generation{Some(original)}else{None}
            }else{
                // Do not publish unknown pre-resync output as a current picture.
                None
            }
        }else{
            self.empty+=1;if super::timing::has_vcl(nal_mask){self.empty_vcl+=1;}else{self.empty_other+=1;}
            self.no_output=self.no_output.saturating_add(1);
            self.peak_no_output=self.peak_no_output.max(self.no_output);
            // Log the first no-output event after warmup; no speculative frame skips.
            if self.seq<=8 || (self.ever_output && self.no_output==1){
                crate::diagnostics::decoder_empty_info(self.seq,self.pending.len(),call_us);
                crate::diagnostics::decoder_cadence_input(self.seq,pts,self.period_us,gap_us,nal_mask,bytes);
            }
            None
        };
        self.peak_pending=self.peak_pending.max(self.pending.len());
        if self.last_report.elapsed().as_secs()>=5{
            let calls=self.outputs+self.empty;
            let lag_x100=self.frame_lag_sum.saturating_mul(100)/self.matched.max(1);
            super::metrics::record_decoder_window(super::metrics::DecoderWindow{
                pending:self.pending.len() as u64,lag_x100,call_us:self.call_total_us/calls.max(1),
                wait_us:self.wait_sum_us/self.matched.max(1),matched:self.matched,
                calls,period_us:self.period_us,over_budget:self.over_budget,at:Some(Instant::now()),
            });
            crate::diagnostics::decoder_buffer_info(self.outputs,self.empty,self.peak_no_output,self.pending.len(),self.peak_pending,self.call_total_us/calls.max(1),self.call_max_us,lag_x100,self.frame_lag_max);
            crate::diagnostics::decoder_cadence_window(self.period_us,self.over_budget,self.matched,self.empty_vcl,self.empty_other);
            crate::diagnostics::decoder_calls_by_result(&self.calls_by_result);
            self.calls_by_result=Default::default();
            self.over_budget=0;self.empty_vcl=0;self.empty_other=0;
            self.last_report=Instant::now();self.outputs=0;self.empty=0;self.peak_no_output=self.no_output;
            self.peak_pending=self.pending.len();self.call_total_us=0;self.call_max_us=0;
            self.frame_lag_sum=0;self.frame_lag_max=0;self.matched=0;self.wait_sum_us=0;
        }
        Ok(result)
    }
}
