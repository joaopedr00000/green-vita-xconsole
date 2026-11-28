//! PS Vita hardware H.264 decoder (`sceVideodec`/`sceAvcdec`).
use super::memory::{CdramBlock, release_reserved_decoder_cdram};
use super::{DecoderConfig, VideoTextureTarget};
use anyhow::{Result, bail};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool,Ordering};
// A failed two-output call disables the experiment until the app restarts.
static DUAL_DISABLED:AtomicBool=AtomicBool::new(false);
use vitasdk_sys::*;

// The idea of reducing the reference frames came from MattKC on his Vanilla project
// Make sure to check it out, good content :)
const AVCDEC_NUM_REF_FRAMES: u32 = 1;
// AVCDEC and SDL's Vita GXM renderer both support RGB565 natively. At 960x544 this halves the
// decoder-to-texture traffic from roughly 2 MiB to 1 MiB per frame, which matters at 60 FPS.
const OUTPUT_BYTES_PER_PIXEL: u32 = 2;
const OUTPUT_PIXEL_FORMAT: u32 = SCE_AVCDEC_PIXELFORMAT_RGBA565 as u32;

struct AvcdecLibrary {
    module_loaded: bool,
}

impl AvcdecLibrary {
    fn initialize(width: u32, height: u32) -> Result<Self> {
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
            numOfRefFrames: AVCDEC_NUM_REF_FRAMES,
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
    pub fn new(config: DecoderConfig) -> Result<Self> {
        unsafe {
            let library = AvcdecLibrary::initialize(config.decode_width, config.decode_height)?;

            let query = SceAvcdecQueryDecoderInfo {
                horizontal: config.decode_width,
                vertical: config.decode_height,
                numOfRefFrames: AVCDEC_NUM_REF_FRAMES,
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
            release_reserved_decoder_cdram();
            let frame_memory =
                CdramBlock::allocate("xcloud_hw_video_frame", decoder_info.frameMemSize)?;
            let mut decoder_control = SceAvcdecCtrl {
                handle: 0,
                frameBuf: SceAvcdecBuf {
                    pBuf: frame_memory.ptr.cast(),
                    size: decoder_info.frameMemSize,
                },
            };
            let ret =
                sceAvcdecCreateDecoder(SCE_VIDEODEC_TYPE_HW_AVCDEC, &mut decoder_control, &query);
            if ret < 0 {
                bail!("sceAvcdecCreateDecoder failed: {ret:#x}");
            }
            let decoder = AvcdecDecoder(decoder_control);

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
        access_unit: &[u8],
        targets: [Option<VideoTextureTarget>;2],
        input_pts:u64,
    ) -> Result<[Option<u64>;2]> {
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
                    pBuf: access_unit.as_ptr() as *mut c_void,
                    size: access_unit.len() as u32,
                },
            };

            let make_picture=|direct_target:VideoTextureTarget|->Result<SceAvcdecPicture>{
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

            Ok(SceAvcdecPicture {
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
            })
            };
            let first=targets[0].ok_or_else(||anyhow::anyhow!("missing first decoder target"))?;
            let mut picture=make_picture(first)?;
            let mut second=match targets[1]{Some(t)=>Some(make_picture(t)?),None=>None};
            let mut pointers:[*mut SceAvcdecPicture;2]=[&mut picture,std::ptr::null_mut()];
            if let Some(p)=second.as_mut(){pointers[1]=p;}
            let count=if second.is_some(){2}else{1};
            let mut array_picture=SceAvcdecArrayPicture{numOfOutput:0,numOfElm:count,pPicture:pointers.as_mut_ptr()};
            let ret=sceAvcdecDecode(&self.decoder.0,&au,&mut array_picture);
            if ret<0 || array_picture.numOfOutput>count {
                if count==2 {
                    DUAL_DISABLED.store(true,Ordering::Relaxed);
                    crate::diagnostics::step("decoder_dual","REJECTED_SINGLE_ON_RECOVERY");
                }
                bail!("sceAvcdecDecode failed or returned invalid output count: {ret:#x}, {}",array_picture.numOfOutput);
            }
            let mut result=[None,None];
            for i in 0..array_picture.numOfOutput as usize{
                let picture=if i==0{&picture}else{second.as_ref().expect("bounded output count")};
                let output_capacity=targets[i].expect("bounded output count").capacity;
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

                result[i]=Some(((picture.info.pts.upper as u64)<<32)|picture.info.pts.lower as u64);
            }
            Ok(result)
        }
    }
}

// SAFETY: the CDRAM blocks and decoder handle have no thread affinity in the underlying SCE API -
// this is only ever moved once (into `VideoDecodeWorker`'s thread), never accessed concurrently.
unsafe impl Send for PublicDecoder {}

use std::time::Instant;
use crate::streaming::hud::DecodeStages;
use super::timing::Pending;
#[derive(Clone,Copy)]
pub(super) struct FrameTiming{pub enqueued:Instant,pub stages:DecodeStages,pub generation:u64}

pub struct HwVideoDecoder {
    backend: PublicDecoder,
    pending: Pending<(FrameTiming,Instant)>,
    seq: u64,
    no_output: u32,
    peak_no_output: u32,
    peak_pending: usize,
    outputs: u64,
    calls: u64,
    dual_calls:u64,
    dual_outputs:u64,
    empty: u64,
    call_total_us: u64,
    call_max_us: u64,
    frame_lag_sum: u64,
    frame_lag_max: u64,
    matched: u64,
    last_report: Instant,
}

impl HwVideoDecoder {
    pub fn new(config:DecoderConfig)->Result<Self>{
        let backend=PublicDecoder::new(config)?;
        crate::diagnostics::step("lab_decoder","PUBLIC");
        Ok(Self{backend,pending:Pending::new(),seq:0,no_output:0,peak_no_output:0,
            peak_pending:0,outputs:0,calls:0,dual_calls:0,dual_outputs:0,empty:0,call_total_us:0,call_max_us:0,frame_lag_sum:0,frame_lag_max:0,matched:0,last_report:Instant::now()})
    }
    pub(super) fn allow_two(&self)->bool{!DUAL_DISABLED.load(Ordering::Relaxed)}
    pub fn decode(&mut self,data:&[u8],targets:[Option<VideoTextureTarget>;2],mut timing:FrameTiming)->Result<Option<(usize,FrameTiming)>>{
        self.seq=self.seq.saturating_add(1);
        let pts=self.seq.saturating_mul(super::AU_PTS_STEP);
        let started=Instant::now();
        let returned=self.backend.decode(data,targets,pts)?;
        self.calls+=1;
        if targets[1].is_some(){self.dual_calls+=1;}
        if returned[1].is_some(){self.dual_outputs+=1;}
        let call_us=started.elapsed().as_micros() as u64;
        timing.stages.decode_us=call_us;
        self.pending.push(pts,(timing,started));
        self.call_total_us=self.call_total_us.saturating_add(call_us);
        self.call_max_us=self.call_max_us.max(call_us);
        let mut latest:Option<(usize,u64,FrameTiming)>=None;
        if returned[0].is_none() && returned[1].is_none(){
            self.empty+=1;self.no_output=self.no_output.saturating_add(1);
            self.peak_no_output=self.peak_no_output.max(self.no_output);
        }else{
            self.no_output=0;
            for (slot,id) in returned.into_iter().enumerate(){
                let Some(id)=id else{continue;};
                self.outputs+=1;
                let original=if let Some((mut original,submitted))=self.pending.take(id){
                    let lag=pts.saturating_sub(id)/super::AU_PTS_STEP;
                    self.frame_lag_sum=self.frame_lag_sum.saturating_add(lag);
                    self.frame_lag_max=self.frame_lag_max.max(lag);self.matched+=1;
                    original.stages.verified=true;
                    original.stages.decoder_age_us=submitted.elapsed().as_micros() as u64;
                    original
                }else{
                    let mut unknown=timing;unknown.stages.verified=false;unknown
                };
                // Never prefer unmatched or pre-resync pictures over confirmed ones.
                if original.generation!=timing.generation {continue;}
                if latest.as_ref().is_none_or(|(_,old_id,old)|
                    (original.stages.verified && !old.stages.verified) ||
                    (original.stages.verified==old.stages.verified && id>*old_id)){
                    latest=Some((slot,id,original));
                }
            }
        }
        let result=latest.map(|(slot,_,mut timing)|{
            timing.stages.decoder_pending=self.pending.len() as u32;
            (slot,timing)
        });
        self.peak_pending=self.peak_pending.max(self.pending.len());
        if self.last_report.elapsed().as_secs()>=5 {
            let calls=self.calls;
            crate::diagnostics::decoder_dual_info(calls,self.dual_calls,self.dual_outputs);
            crate::diagnostics::decoder_buffer_info(self.outputs,self.empty,self.peak_no_output,
                self.pending.len(),self.peak_pending,self.call_total_us/calls.max(1),self.call_max_us,
                self.frame_lag_sum.saturating_mul(100)/self.matched.max(1),self.frame_lag_max);
            self.last_report=Instant::now();
            self.outputs=0;self.calls=0;self.dual_calls=0;self.dual_outputs=0;self.empty=0;self.peak_no_output=self.no_output;
            self.peak_pending=self.pending.len();self.call_total_us=0;self.call_max_us=0;
            self.frame_lag_sum=0;self.frame_lag_max=0;self.matched=0;
        }
        Ok(result)
    }
}
